"""The Mac's end of the NCM probe (run.sh): it answers the headset's multicast to ff02::1, serves a
TCP test and prints what changes in the network interfaces and in adb while the probe runs. It
prints the headset's log lines that arrive over Wi-Fi as well.

Usage: mac_side.py SECONDS SERIAL"""
import os
import select
import socket
import struct
import subprocess
import sys
import threading
import time

PORT = 9963
LOG_PORT = 9964
SERIAL = sys.argv[2]
t0 = time.time()


def say(text):
    print(f"{time.time() - t0:8.3f} {text}", flush=True)


def interfaces():
    return set(subprocess.run(["ifconfig", "-l"], capture_output=True, text=True).stdout.split())


# Two sockets share the port: one that joined ff02::1 on each interface and one that did not, so
# the log shows whether macOS hands the multicast to a socket that never joined the group.
def udp_socket():
    s = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
    s.bind(("::", PORT))
    return s


def join(s, name):
    try:
        index = socket.if_nametoindex(name)
        mreq = socket.inet_pton(socket.AF_INET6, "ff02::1") + struct.pack("@I", index)
        s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_JOIN_GROUP, mreq)
        say(f"joined ff02::1 on {name} ({index})")
    except OSError as e:
        say(f"join on {name} failed: {e}")


def udp_loop(plain, joined):
    while True:
        ready, _, _ = select.select([plain, joined], [], [], 1.0)
        for s in ready:
            data, sender = s.recvfrom(2048)
            which = "plain" if s is plain else "joined"
            text = data.decode(errors="replace")
            if not text.startswith("NCMPING"):
                say(f"udp[{which}] from {sender}: {text}")
            reply = (b"NCMPROBE! " + socket.gethostname().encode()) if text.startswith("NCMPROBE?") else data
            s.sendto(reply, sender)


def tcp_loop():
    srv = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
    srv.bind(("::", PORT))
    srv.listen(4)
    while True:
        conn, peer = srv.accept()
        conn.settimeout(5)
        try:
            line = conn.recv(64).decode(errors="replace").strip()
            say(f"tcp from {peer}: {line}")
            if line == "blast":
                chunk = b"\0" * 65536
                end = time.time() + 3
                sent = 0
                while time.time() < end:
                    sent += conn.send(chunk)
                say(f"tcp blast sent {sent} bytes = {sent * 8 / 3e6:.1f} Mbps")
            else:
                conn.sendall(b"hi\n")
        except OSError as e:
            say(f"tcp error {e}")
        finally:
            conn.close()


def log_loop():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", LOG_PORT))
    while True:
        data, _ = s.recvfrom(4096)
        say("quest| " + data.decode(errors="replace"))


def adb_state():
    out = subprocess.run(["adb", "devices"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        if line.startswith(SERIAL):
            return line.split()[1]
    return "absent"


def main():
    plain = udp_socket()
    joined = udp_socket()
    known = interfaces()
    for name in sorted(known):
        if name.startswith("en"):
            join(joined, name)
    threading.Thread(target=udp_loop, args=(plain, joined), daemon=True).start()
    threading.Thread(target=tcp_loop, daemon=True).start()
    threading.Thread(target=log_loop, daemon=True).start()
    adb = adb_state()
    say(f"ready, adb {adb}, interfaces {' '.join(sorted(known))}")
    end = time.time() + float(sys.argv[1])
    while time.time() < end:
        now = interfaces()
        for name in sorted(now - known):
            info = subprocess.run(["ifconfig", name], capture_output=True, text=True).stdout
            say(f"new interface {name}:\n{info}")
            join(joined, name)
        for name in sorted(known - now):
            say(f"interface gone {name}")
        known = now
        state = adb_state()
        if state != adb:
            say(f"adb {adb} -> {state}")
            adb = state
        time.sleep(0.5)
    say("done")


main()
