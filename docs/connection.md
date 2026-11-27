# Connection

## Finding the Mac

The connect panel lists the Macs that run the cranvr host. The client broadcasts a query by UDP to port 9950 (`proto/src/discovery.rs`). It sends it to `255.255.255.255`, to the broadcast address of its own /24 network and to this machine. Each host answers the sender: the Mac's name, the TCP port of the session, the start of the host key's SHA-256 fingerprint and the protocol version that it speaks. The client lists the answers and forgets a Mac that has not answered for five seconds. A press on a Mac connects to the address that the answer came from, at the port in it. A query is longer than any answer, so a host cannot serve to amplify a spoofed query. The answer proves nothing: the handshake proves the host's key. Several hosts on one Mac share the UDP port (`SO_REUSEPORT`). `--discovery-port N` changes the port. 0 turns the answers off. A router that drops broadcasts leaves the list empty. The address field is the way around it.

## Pairing

A headset on another machine needs the Mac's user to allow it once. The client makes a key for itself (Ed25519) and keeps it in Cranpose's preferences service. The host makes a key at its first start and keeps it in `host-identity.txt` in the config folder (readable by the user alone).

1. The user presses the Mac in the headset's connect panel. The handshake proves the headset's key. The host does not know it, so it holds the connection and sends `AwaitApproval`.
2. Both sides compute a code of six digits. The headset's panel shows it with "Allow it on the Mac". The host lists the headset in its `native` status line, under `pending`, with the key, the name, the address and the code. The cranvr app shows a card: "Meta Quest 3 wants to connect. It shows the code 482 913." with Allow and Deny. The app also brings its window to the front.
3. The user compares the two codes and presses Allow. The app writes `allow KEY` to the host's input (`deny KEY` for Deny). The host writes the headset's public key to `paired-headsets.txt` in the config folder and starts the session on the held connection.
4. A later connection needs no answer. The host remembers the headset's key and the headset remembers the host's, under the address and under the fingerprint, so a new address of the Mac does not make a new pairing.

The host holds one request at a time: another unknown headset gets the reason `Busy`. A request ends with the answer, after three minutes (the headset hears `NoAnswer`) or when the headset closes the connection. A person who runs the host by hand with `--status` types `allow KEY` or `deny KEY` into its input. Without `--status` the host leaves its input alone, because a background job that reads its terminal would stop. `--trust-all` accepts every client without pairing, for tests. A client on this Mac connects without pairing. So does a headset over USB through `adb reverse`, because its connection comes from the loopback address.

Each connection proves both keys. A headset refuses a host whose key differs from the one that it remembers and says so in the connect panel. A new install of the host that lost `host-identity.txt` has a new key: press "Forget host key" in the connect panel (or remove the preference `host-key:ADDRESS`), then connect again.

### The code

The code protects the Mac. A machine that poses as the Mac to the headset and as the headset to the Mac holds two connections with two sets of keys. The two screens must show two different codes, so the user denies the headset. The code is the SHA-256 of the handshake's transcript (both preambles, both fresh keys and the host's key), the headset's key and two random numbers, one from each side, taken mod a million.

A short code alone would not hold: the machine in the middle could try a million fresh keys until the two codes agree. So the host commits first. `AwaitApproval` carries a hash of the host's number, the transcript and the headset's key. The headset answers with its own number (`PairNonce`). Then the host reveals its number (`PairReveal`). The headset checks it against the hash. Neither side can steer the code. A machine in the middle ends with equal codes with a chance of one in a million. `proto/tests/secure.rs` runs such a machine against a real host and client.

### Versions

The protocol has one version, `PROTOCOL_VERSION` in `proto/src/lib.rs`. cranvr has no public release, so it supports no other version. Each side sends a preamble first. The client's has six bytes: the magic `CRNV` and the version. The host's has seven: the magic, the version and whether the host takes a link over the USB cable (see "The USB cable"). A client reads the host's first six bytes, and the seventh only from a host of its own version. A peer of another version gets no session. The host answers with its own preamble and closes the connection. Each side then names the side to update, which is the side with the lower version: "Update cranvr on the Mac." or "Update the cranvr app on the headset." The host shows it in a `network` status line (`incompatible_client`). The discovery answer carries the host's version, so the connect panel shows the advice on the Mac's entry before a press. A change of a message, of the handshake or of a datagram raises the version. Both apps are built from one commit. Version 11 has the host's word on the cable in its preamble. Version 9 has the displays of the Mac ("Several displays"), the host's word that a game runs and the hands that a panel holds back from the game ("Panels in a game and the hands"). The first display has no messages of its own: every display, the first included, uses `DisplayWish`, `DisplayStream`, `DisplayMouse` and `DisplayCursor` with its stream.

## The USB cable

A headset on a USB cable at the Mac connects over the cable without a typed address. Two ways run over the cable. The headset's USB network needs no Developer Mode. `adb reverse` needs Developer Mode. The headset prefers the USB network when both answer.

**The USB network** (`client/src/usb_network.rs`). The headset app asks Horizon OS for a network of the USB transport without the internet (Cranpose's `request_network`). The headset then switches its USB functions to NCM (2833:5013 becomes 2833:5018; adb stays in the new functions). macOS makes an interface for it, such as `en10`, with an IPv6 link-local address. The headset sends the discovery query to `ff02::1` on its interface `usb0`. The host answers from its IPv6 responder, which joins no multicast group: macOS hands every datagram of its port to such a socket, also on an interface that appears after the host started. The host listens on `[::]:PORT` beside `0.0.0.0:PORT`. The headset binds its sockets to the network and connects to the Mac's link-local address with its scope. The media goes over UDP with parity. The client tells the host in its capabilities that the link runs over the cable (`ClientCaps::cable`, protocol version 10), so the host starts the eye stream at the user's bitrate (see "The eye stream's bitrate"). A headset that the Mac does not know pairs with a code once, as over Wi-Fi, because the Mac cannot tell a link-local peer on a cable from one on another network. The headset asks while it has the focus. Horizon OS keeps a released network for about 30 s before it switches the USB functions back. The first time, macOS asks "Allow accessory to connect?" for the new USB functions. The link waits for that answer. `--ez usb_network false` keeps the USB functions as they are for one launch.

Measured on a Quest 3 with a USB 2 cable (480 Mbps, `scripts/e2e.sh --quest SERIAL --cable usb-network|adb --native-port 9961 --seconds 30`, the host's test scene at 120 Hz, two runs of each way in turn):

| | USB network | `adb reverse` |
|---|---|---|
| fps (median) | 119.99, 119.96 | 119.87, 119.98 |
| late frames (median) | 0.17 %, 0.33 % | 2.33 %, 0.17 % |
| pose to display (median) | 32.9 ms, 33.0 ms | 34.2 ms, 33.0 ms |
| network time (median) | 1.23 ms, 1.20 ms | 1.96 ms, 1.96 ms |
| sent (median) | 34.6 Mbps, 33.6 Mbps (16 to 19 % of it parity) | 28.2 Mbps, 28.2 Mbps |
| media | UDP | TCP |

Both ways hold 120 fps. The USB network saves about 0.7 ms of network time. The late frames and the time from the pose to the display differ within the noise of two runs. A probe of the link moved 324 to 326 Mbps from the Mac to the headset over TCP.

**`adb reverse`.** The cable gives the stream TCP and a steady link. The Mac looks for the adb tool (the PATH, Android Studio's SDK, Homebrew's android-platform-tools). A Mac with none shows "Add USB support" in the Settings tab, which downloads Google's platform-tools into cranvr's folder. The Mac counts a device only when `dev.cranvr.client` or the phone build is installed on it. The app then sets up `adb reverse` for the host's port. The headset lists "This Mac by USB cable" first (`app/src/usb.rs`, `client/src/usb.rs`, `cranvr_proto::link::probe_host`). The Settings tab has a Connection choice: Automatic, Wi-Fi or USB cable. When the cable comes out, the headset connects to the same Mac over Wi-Fi by itself after about three seconds. A Wi-Fi link never moves to the cable while it runs.

**The choice of Wi-Fi** stops both ways over the cable. The app takes its `adb reverse` away and writes `cable off` to the host's input (`--no-cable` when it starts the host). The host's preamble then says that it takes no link over the cable, on its IPv4 and its IPv6 listener. The headset's question over the cable still gets the Mac's key with that answer. The headset lists no cable line for that Mac and connects to it over Wi-Fi: the key finds the Mac's Wi-Fi entry, also for a launch at `127.0.0.1`. A headset that comes over the cable anyway, because it asked a moment before the change, hears `WiFiOnly` and connects again over Wi-Fi. Automatic and USB cable write `cable on`. A change counts from the next connection: a running stream keeps its path. The headset keeps its request for the USB network, so the cable's way comes back without a new switch of its USB functions.

## The sealed session

The protocol encrypts and authenticates everything after the version exchange.

The handshake (`proto/src/secure.rs`, `proto/src/link.rs`) has three flights after the preambles:
1. The client sends a fresh X25519 key.
2. The host answers with its fresh key, its Ed25519 key and its signature over the transcript. The transcript hashes both preambles (a downgrade of the version breaks it), both fresh keys and the host's key. The client checks the signature and, when it remembers a key of this host, compares the keys.
3. The client sends its Ed25519 key and its signature over the same transcript, already sealed. The host verifies it. The signature covers the host's fresh key, so a recorded proof does not serve in another session.

HKDF-SHA256 turns the X25519 secret into four keys (salt: the transcript): the TCP path and the UDP path, each in two directions. ChaCha20-Poly1305 seals each packet. The nonce is four zero bytes and the packet's counter.

| Path | What is on the wire | Order and replay |
|---|---|---|
| TCP | 4-byte length, 8-byte counter, content, 16-byte tag. It carries the control messages. With TCP media (USB) it carries the media and the datagrams too. | The counter must be the next one. A record that fails to open, repeats or is missing ends the session. |
| UDP | 8-byte counter, content, 16-byte tag. Carries the media and the datagram messages. | A window of 4096 counters tolerates loss and reordering. A replayed or older packet and one that fails the tag are dropped like a lost one. |

The sender computes the parity over the plain packets and seals each packet on its own, so a lost packet stays recoverable. The sealed datagram keeps the 1400-byte limit, so the media payload of a packet is 1352 bytes. The host's UDP socket learns the client's address from the first datagram that opens with the session's key. The sealing hides the content. The sizes and the times of the packets and the addresses stay visible.

Sealing and opening a packet of 1400 bytes takes about 1.9 us on an M5 (0.75 GB/s). Sealing the frames of a stream of 100 Mbps at 120 Hz takes about 2 percent of one core. `cargo test --release -p cranvr-proto --test secure seal_cost -- --ignored --nocapture` measures it. The client's stats and its log line show the opening time per frame, per packet and as a share of one core.

## Display lock

The headset shows a new frame at each point of a grid, 120 times a second. Its compositor takes decoded frames some milliseconds before that point (about 16 ms on a Quest 3). A host that renders at the arrival of each pose has a fixed delay from pose to arrival. Jitter puts two frames into one interval and none into the next.

The host plans each frame of its own scene (`host/src/headset/display_lock.rs`). Each tracking sample carries the display time that its poses are predicted for. The times of successive samples give the host the headset's display grid. The compositor's reports give the lead (display time minus pick time) and how long a frame takes from its start to the end of its decode. For each frame the host
- picks the target, the grid point at which the frame should show;
- starts the frame so that its decode ends a margin before the pick of that point. The margin comes from the 90th percentile of the measured time and an extra that rises by 0.2 ms when a frame misses and falls by 0.005 ms while frames hit, so it rests at about 2.4 percent of misses;
- renders for the pose that is valid at the target, which it gets by moving the sample's pose with its velocities (at most 80 ms ahead);
- puts the target in the frame's metadata (`FrameMeta::target_display`, on the client's clock).

A planned frame starts at its planned time, or earlier at the arrival of a pose. The frame renders for the newest pose at its planned start. A pose that arrives less than a period minus half a millisecond before that start is the last one before it, because the next pose comes one period later. The frame then starts at once with the same pose and has the time up to the planned start as extra. The margin falls while the frames hit, so the plans move later until the misses return to their share. The wait for a planned start runs on a timer until 1.5 ms before the start and spins from there, because a timer wakes the thread up to a millisecond late. The time from a frame's start to the end of its decode is counted from the start that the frame took.

When no newer pose came since the last frame, a planned frame renders again for the same sample, with the pose moved on to the new target. Such a frame carries the sample's time plus one or two nanoseconds, so that the notes, the encoder and the headset's reports find it. At most two frames in a row repeat a sample.

The client passes the target through: `Hub::views_of` turns it into the runtime's clock and puts it in `StreamedViews::target_display_time`. The scene then shows each frame at its point. A frame that comes early waits in the queue. A frame that comes after the take for its point is late and is skipped. While a game runs, the game sets the pace and the lock does not plan the start of a frame. A frame that no plan started, such as a game's frame, takes the first point of the lock's grid at or after its arrival plus the lead (`DisplayLock::point_for`). `--no-display-lock` turns the lock off for a comparison.

The probe models the headset's queue (`probe/src/compositor.rs`: the display grid, the take one lead before each point and Cranpose's queue of three frames). Its report line shows how many frames the queue showed, skipped and repeated. It also shows how many frames came after their own point.

## Network exposure

- The host listens on TCP 9950 (`--native-port`) on all interfaces. A client that the host does not know waits for the user's Allow and gets no stream before. Everything after the handshake is sealed.
- The host answers discovery queries on UDP 9950 (`--discovery-port`) on all interfaces.
- The game link listens on 127.0.0.1 only. A game must show the token from `runtime-control.txt` (mode 0600).

Use pairing for real use. `--trust-all` accepts any client on the network and is meant for tests.
