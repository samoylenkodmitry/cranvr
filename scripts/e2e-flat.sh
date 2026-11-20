#!/bin/zsh
# The client's side of the zero cost of a game with nothing shown. It runs a test host with the
# fake game and the client in a flat window (the window opens on this Mac's screen for the length of
# the run), and reads the logs of both.
#
# Usage: scripts/e2e-flat.sh [--seconds N] [--native-port PORT]
#
# The run fails unless, while the game runs and nothing shows:
#   - the host's log has 3 or more windows of one second with a game that runs, no display shown and
#     0 pictures captured, 0 encoded, 0 sent and 0 cursor datagrams;
#   - the last line "layers composed by the app" says 1 panels and 0 display surfaces: the ball only,
#     which is what the client composed in a game before the hand menu existed.
# A flat window runs no display's decoder at all (the displays work in a headset only), so the check
# that a headset decodes 0 pictures belongs to a run on a headset or the Android XR emulator
# (`displays: 0 wished; ... 0 pictures received, 0 sent to a decoder` in its log).
# It does not touch the owner's host, app or client files: the host's folder and the client's HOME are
# new, and the port is a test port.

setopt NO_UNSET PIPE_FAIL
root=${0:A:h:h}
seconds=24
port=""
while (( $# )); do
    case $1 in
        --seconds) seconds=$2; shift 2 ;;
        --native-port) port=$2; shift 2 ;;
        *) print -u2 "unknown option $1"; exit 2 ;;
    esac
done
if [[ -z ${port} ]]; then
    for candidate in 9961 9962; do
        lsof -nP -iTCP:${candidate} -sTCP:LISTEN >/dev/null 2>&1 || { port=${candidate}; break; }
    done
fi
[[ -n ${port} && ${port} != 9950 ]] || { print -u2 "no free test port"; exit 3; }

(cd "${root}" && cargo build --release -p cranvr-host -p cranvr-probe >/dev/null 2>&1) || { print -u2 "the host did not build"; exit 4; }
(cd "${root}/client" && cargo build --release --bin cranvr-client >/dev/null 2>&1) || { print -u2 "the client did not build"; exit 4; }

work=${root}/target/e2e/flat-$(date +%Y%m%d-%H%M%S)
cfg=${work}/config
mkdir -p "${cfg}" "${work}/home"
export CRANVR_DISPLAY_TEST=main CRANVR_POINTER_DRY_RUN=1
"${root}/target/release/cranvr-host" --status --config-dir "${cfg}" --fps 72 --native-port ${port} --desktop --trust-all > "${work}/host.out" 2> "${work}/host.log" &
host=$!
game=0
client=0
cleanup() {
    (( client )) && kill ${client} 2>/dev/null
    (( game )) && kill ${game} 2>/dev/null
    kill ${host} 2>/dev/null
    return 0
}
trap cleanup EXIT
for i in {1..50}; do [[ -f ${cfg}/runtime-control.txt ]] && break; sleep 0.2; done
[[ -f ${cfg}/runtime-control.txt ]] || { print -u2 "the host made no runtime control file (see ${work}/host.log)"; exit 5; }
HOME=${work}/home RUST_LOG=info "${root}/client/target/release/cranvr-client" --host=127.0.0.1:${port} --connect > "${work}/client.log" 2>&1 &
client=$!
# The fake game starts after the client has connected, as a game starts after the user connected
for i in {1..100}; do grep -q "decoder started\|fps received" "${work}/client.log" 2>/dev/null && break; sleep 0.2; done
sleep 2
"${root}/target/release/cranvr-fake-game" --control "${cfg}/runtime-control.txt" --fps 72 --seconds $(( seconds )) > "${work}/game.log" 2>&1 &
game=$!
sleep $(( seconds ))
cleanup
wait 2>/dev/null

print "logs in ${work}"
python3 - "${work}" <<'EOF'
import re, sys
work = sys.argv[1]
host = open(f"{work}/host.out", errors="replace").read().splitlines()
client = open(f"{work}/client.log", errors="replace").read().splitlines()
problems = []

quiet = [l for l in host if re.search(r"Native displays: \d+ known, 0 shown, a game runs; in the last [\d.]+ s: 0 pictures captured, 0 encoded, 0 sent, 0 cursor datagrams sent", l)]
busy = [l for l in host if "a game runs" in l and "Native displays" in l and not re.search(r"0 pictures captured, 0 encoded, 0 sent, 0 cursor", l)]
print(f"host: {len(quiet)} windows with a game that runs, no display shown and nothing captured, encoded or sent; {len(busy)} windows of a game with a display's cost")
if len(quiet) < 3:
    problems.append("the host's log has fewer than 3 quiet windows of a game that runs")
if any("0 shown" in l for l in busy):
    problems.append("the host captured, encoded or sent for a display while none was shown")

layers = [l for l in client if "layers composed by the app" in l]
for l in layers:
    print("client:", re.sub(r"^\S+ \S+ ", "", l))
if not layers:
    problems.append("the client logged no layer count")
elif not re.search(r"1 panels and 0 display surfaces", layers[-1]):
    problems.append("the last layer count is not the ball only: " + layers[-1])

print("e2e-flat: " + ("FAIL: " + "; ".join(problems) if problems else "PASS"))
sys.exit(1 if problems else 0)
EOF
