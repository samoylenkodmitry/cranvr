# cranvr

cranvr streams VR from an Apple Silicon Mac to a Meta Quest. The Mac side is Rust. It renders and composes with Metal, encodes HEVC with VideoToolbox and sends the frames with cranvr's own protocol (`proto/`, crate `cranvr-proto`). The headset side is a Cranpose app (`client/`, package `cranvr-client`, application id `dev.cranvr.client2`). It uses only Cranpose's public API and plain Rust crates. It has no ndk, JNI, GL, OpenXR or platform code of its own. The same crate also builds as a flat app for an Android phone or tablet.

The goal is to play Windows SteamVR games on a Quest from a Mac. Windows games run under Wine with the OpenXR runtime and the DXMT fork from [BasaltVR](https://github.com/Stooovie/BasaltVR) (MIT). The game's frames reach the host as shared IOSurfaces. No frame passes through the CPU.

## What works

- A head-tracked test scene for a headset on a desk.
- Foveated encoding. The host encodes about a third of the pixels of an eye. The client unwarps them. It makes the full Quest 3 eye size fit at 120 Hz.
- Windows OpenXR games under Wine. Head pose, controller poses, buttons and haptics pass both ways.
- The Mac desktop as a panel in the headset, with the controller as the mouse. The Mac's sound plays in the headset.
- A display lock. The host starts each frame of its test scene so that it reaches the headset's display on its grid.
- Discovery of the Mac by the headset, a pairing that the user allows on the Mac, an encrypted and authenticated session and a USB cable link.
- The `cranvr` window (`app/`, built with Cranpose). It starts the host, asks the user to allow a new headset, sets up the games and starts them.
- An end-to-end harness, `scripts/e2e.sh`, with a test client on this Mac or a Quest. The host reports the client's latency by stage.

## Parts

| Folder | What it does |
|---|---|
| `vt` | HEVC encoder and decoder through plain C bindings to VideoToolbox. |
| `common` | The foveation math and the ids of the controller inputs, for the host and the probe. |
| `proto` | `cranvr-proto`: the handshake, the messages, the media packets with Reed-Solomon parity, the clock sync, the discovery and the keys. It builds for macOS and for Android. |
| `host` | `cranvr-host`: the frame loop, the Metal shaders (`host/src/pattern.metal`), the headset link (`headset/native.rs`), the game link (`runtime_link.rs`) and the desktop (`desktop.rs`). |
| `app` | `cranvr`: the window. It runs `cranvr-host` and shows its status. |
| `probe` | `cranvr-probe` (a client of the protocol on this Mac for tests) and `cranvr-fake-game` (a stand-in for a game). |
| `client` | `cranvr-client`: the Cranpose app for the Quest and for Android phones. Its own Cargo workspace. |
| `scripts` | Build the client, bundle the app, set up Wine and Steam, run games, run the end-to-end test. |
| `third_party/basaltvr` | cranvr's patches to BasaltVR's runtime and DXMT fork. |

## Build

```bash
cargo build --release
scripts/build-client.sh            # target/client/cranvr-client.apk
scripts/build-client.sh --install  # and install it with adb
scripts/build-client.sh --phone    # cranvr-phone.apk for an Android phone or tablet
```

`scripts/bundle-app.sh --install` builds `cranvr.app` and installs it in `/Applications`.

## Run

1. Put the Mac and the headset on the same network. Use 5 GHz or 6 GHz Wi-Fi, or the USB cable.
2. Start the window: `./target/release/cranvr`. The host starts by itself.
3. Open cranvr on the headset. The connect panel lists the Macs that run cranvr. Press yours. A headset that the Mac does not know shows a code of six digits. The window on the Mac shows the same code with Allow and Deny. Press Allow when the two codes are the same.

`docs/quest-first-run.md` is the checklist for a first run with Steam and Beat Saber. The window saves its settings in `~/Library/Application Support/cranvr/app.json`. A change of refresh rate, bitrate, foveation or desktop restarts the host. The headset reconnects by itself.

Without the window:

```bash
./target/release/cranvr-host --fps 120 --desktop
```

`cranvr-host --help` lists every option. `--status` prints status lines (`@cranvr` and a JSON object) on stdout for a program that runs the host. `host/src/status.rs` lists them.

### What you should see

With the desktop off, the host shows a test scene:
- a sky with a grid line every 15 degrees and brighter lines every 45 degrees;
- a floor with 50 cm squares and a green line every meter;
- a yellow ball at the left controller and a cyan ball at the right controller;
- a small red square near the left edge of the left eye and a blue square near the left edge of the right eye.

A game replaces the scene while it runs. The headset's Room setting (in its console) decides what fills the view while no game runs. With Passthrough (the default) the room shows around the panels and the client leaves the streamed scene out. With Scene the host's picture fills the view. The launch argument `room` (`scene` or `passthrough`) decides for one launch.

## The headset app

`client/` holds the app. In a headset the panels are movable panels in the room. On a desktop they draw side by side in a flat window.

- The connect panel lists the Macs that it found, takes a typed address and shows the state of the link.
- The stream is a `StreamedScene`. Cranpose's `VideoDecoder` outputs frames. The scene keeps up to three decoded frames in a queue. At each headset frame it shows the frame whose target display time lies closest to the display time of that headset frame. The headset's compositor shows each frame from the pose that the host rendered it for and corrects it for the head's newer pose.
- The stats panel shows the frame rate, the decode time, the loss, the FEC recoveries, the sound and the latency. It also shows what the scene did with the decoded frames: how many it showed a second, how many it skipped and how many headset frames showed the last picture again.
- A `SpatialFrameEffect` runs once for each headset frame. It hands the head pose, the eye views, both controllers and the predicted display time to the thread that sends the tracking.

The launch arguments are intent extras on Android (`adb shell am start -n dev.cranvr.client2/dev.cranpose.android.CranposeActivity --es host 192.168.1.20:9950 --ez connect true`) and `--name=value` on a desktop (`--host=192.168.1.20:9950 --connect`):

| Argument | Meaning |
|---|---|
| `host` | The Mac's address and port for the text field. |
| `connect` | Connect at once. |
| `media` | `udp` or `tcp`: the media path. |
| `room` | `scene` or `passthrough`, for this launch. |
| `console` | Open the console and keep it open, for a headset on a desk. |
| `usb_port` | The port that the headset asks on the USB cable, for a test host on another port. |
| `desktop`, `desktop_width`, `cursor`, `desktop_pointer_test` | The desktop panel: `desktop false` asks for none, `desktop_width` is the widest picture to ask for. |

The client's modules: `connection.rs` runs the link on its own threads. `hub.rs` hands the numbers to the UI. `tracking.rs` turns Cranpose's `XrFrame` into the protocol's messages. `ui.rs` holds the panels, `console.rs` and `console_panel.rs` the console, `desktop.rs` the desktop panel, `audio.rs` the sound, `usb.rs` the cable and `phone.rs` the phone mode.

### Tracking

- The Hello carries the system name, the eye size to ask for, the refresh rates of `HeadsetProperties`, the fields of view and the eyes' places in the head. A change of the distance between the eyes goes to the host as a `Views` message.
- The runtime's display time and clock reading are on one clock. The client adds their difference to its own clock reading, so the display time lands on the client's clock.
- Each headset frame sends one tracking sample: the head pose, the grip pose and the aim pose of each tracked controller, each with its velocities. It also carries the display time that the poses are predicted for. The input of both hands (buttons, touches, trigger, grip, thumbstick) goes as a datagram with every sample, so a lost datagram leaves a button wrong for one frame at most.
- A recenter of the headset goes to the host as a `Recenter` message. The host then puts its desktop screen in front of the user again.
- The floor goes to the host as a `Floor` message. It comes from the stage that the runtime gives. The poses are in the app's room (OpenXR's LOCAL space), whose origin lies where the head was when the app started. The host moves the room up by the height of the floor before it gives the poses to its test scene or to a game, so the floor lies at height 0 there.
- The host's haptic pulses play on the controllers through Cranpose's `Haptics`.
- Hands, body, face and eyes (`tracked.rs`): the Quest build asks Cranpose for the four kinds of tracking in `build.rs` (`Use::hand_tracking()` and its siblings). An OpenXR instance fixes its extensions before the app runs, so the declaration cannot wait for a game. It writes the permissions to the manifest, and Cranpose asks the user for them before the OpenXR instance exists. The headset offers a kind to the host (`ClientMsg::TrackingOffer`) only while its tracker runs, which is after the user allowed it. A Quest 3 has no eye cameras, so it never offers the eyes. Its face comes from the microphone, which Cranpose calls the audio data source. The offer names the source of the face (`Features::FACE_AUDIO` or `FACE_VISUAL`), and each face sample carries its own. Cranpose names the source per frame, so the client offers a face of an unknown source until a valid sample names it. A face from Android XR's own extension has the 68 weights of Jetpack XR. The client maps them to Meta's 70 weights by `FaceBlendShapeType::expression()` and leaves the weights that Android XR lacks at zero. It offers that face as a visual one, because Android XR reads the face with the headset's cameras. The client sends a feature only while the host wishes it (`HostMsg::TrackingWish`): one datagram per headset frame, from a buffer that it allocated once. Cranpose gives no velocities for joints, so the client estimates the velocity of each wrist and of the body's root from the poses of two frames, as it does for the head.

### The eye size and the decoder

The client asks for the headset's recommended eye size. The host takes it unless `--eye-width` says otherwise. The client asks Cranpose for the decoder's limits (`VideoDecoder::capabilities`) before the Hello. It asks for the recommended size unless the frame that the host would encode lies beyond the decoder's largest picture or beyond its frame rate at the refresh rate. It then takes a smaller eye, down to half the recommended size. A client that takes no foveation gets a smaller eye: 1248 pixels at 120 Hz, 1504 at 90 Hz and 1728 at 72 Hz, unless `--eye-width` says otherwise.

### The console

A hold of the left menu button (0.8 seconds) shows or hides the console, a movable panel with the games to play and the main settings, below and ahead of the head and tilted up like a desk. A shorter press goes to the game only. A button row at the bottom has Close, Disconnect (while the link runs) and Exit. The console shows when the first menu arrives and while no game runs. It hides when a game starts.

The cranvr app writes the games and the settings to `vr-menu.json` and a one-line `notice` while it has something to say. The host adds the stream's state as `status` and sends the menu as `HostMsg::Menu` when the client connects and when it changes (at most once a second). A press sends `ClientMsg::Console` with the request as JSON (`{"action": "play", "game": "Beat Saber"}`, `{"action": "stop"}`, `{"action": "set", "fps": 90}`). The host hands it to the app as a `vr_request` status line. The app plays the game or changes the setting. A changed refresh rate or game size restarts the host. The client then connects again by itself every 2 seconds until the stream is back, unless the user pressed Disconnect.

The console has two settings of the client itself, which never go to the host: Room (Passthrough or Scene) and Volume (100%, 150% (the default), 200% or 300%). Cranpose's preferences service saves them. The audio renderer multiplies each sample by the volume. A soft limiter above 0.8 keeps loud parts below 1.

### Phone mode

`scripts/build-client.sh --phone [--install SERIAL]` builds `target/client/cranvr-phone.apk` from the same crate. Its application id is `dev.cranvr.client2.phone` and its label is "cranvr phone". The build differs from the headset's in four places: the Cargo feature `phone` replaces `android` (the entry is `cranpose::android_main!`), the build has no OpenXR loader and no `xr` service, the Gradle property `cranvrPhone=true` switches the module and the phone's manifest turns the activity to landscape with the sensor. `cargo run --manifest-path client/Cargo.toml -- --phone --host=127.0.0.1:9950 --connect` runs the phone's app in a desktop window.

- The picture fills the screen. A tap opens the connect panel and the stats panel over it. The Hide panels button closes them. They open again when the stream ends. The Recenter button puts the host's front where the screen points.
- The head comes from the device's motion sensors (`client/src/phone.rs`). The first look along the horizon becomes the host's front. The head stays at a height of 1.6 m.
- The Hello carries one view shape for both eyes. It spans 90 degrees across the screen's long side. The host renders two eyes as usual and the phone shows the left one. Cranpose has no public call for the display's refresh rate, so the client measures it.
- A phone's scene shows the newest frame and Cranpose reports nothing about it. The client sends the frame timing when the frame arrives: the picture is ready one mean decode time after the last packet, the next screen frame takes it and that frame reaches the display one period later. The host's `total_ms` and `horizon_ms` rest on these estimates.
- The phone has no desktop panel, no console and no controller emulation.

### Latency numbers

The host's stats line carries a `latency` block with the averages of the client's frame timing reports. The client reports for each frame that the streamed scene took: the tracking sample, the last packet's arrival, the end of the decode, the take by the headset's frame loop and the display time of the headset frame that shows the frame. The display time is the frame's own point on the headset's display grid, computed when the frame enters the scene's queue. A frame that the scene skips still reports one.

| Field | From, to |
|---|---|
| `total_ms` | the tracking sample to the display time |
| `pose_to_send_ms` | the tracking sample to the start of the host's send: the uplink, the wait for the planned start, the render and the encode |
| `network_ms` | the start of the send to the last packet |
| `decode_ms` | the last packet to the end of the decode |
| `decode_queue_ms` | the end of the decode to the take |
| `vsync_wait_ms` | the take to the display time |

The headset samples its poses at the start of a headset frame, 17 ms before the display time that the poses are predicted for. A frame shows at a point of the display grid. So `total_ms` of a frame is 17 ms plus a whole number of periods: 42 ms for three periods, 50 ms for four. Most frames of the test scene show after three. A frame shows after four when its pose reached the host too late for the plan's start, and after more when the pose never came. The mean falls when more frames make the three, which needs the host's time from the start to the end of the decode (about 14 ms at the median and 20 to 26 ms at the 97.5th percentile) plus the lead (16.3 ms) to fit between the pose's arrival and the point. `decode_queue_ms` is the time that a frame that made its point waits for the headset's take. A frame that starts earlier at the arrival of its pose waits longer and shows at the same point. The wait therefore says little about the latency.

The numbers below come from `scripts/e2e.sh --quest SERIAL --cranvr-client --native-port 9962 --seconds 30` on a Quest 3 over Wi-Fi at 120 Hz with the test scene. Each row is one run: the mean over its stats lines after the first. The load is the Mac's one-minute load average at the start and at the end of the run, with other programs running. `late` and `replaced` are the host's shares of frames that showed after their point and of frames that the headset replaced with a newer one. `shown` and `skipped` are the client's frames per second that the scene showed and skipped.

| Run | `total_ms` | `decode_queue_ms` | late | replaced | shown | skipped | load |
|---|---|---|---|---|---|---|---|
| 1 | 43.07 | 9.43 | 2.43% | 0.17% | 116.4 | 2.8 | 7.9, 6.9 |
| 2 | 43.44 | 9.72 | 1.90% | 0.24% | 117.6 | 2.0 | 7.4, 6.6 |
| 3 | 42.25 | 9.04 | 1.94% | 0.07% | 118.0 | 1.9 | 5.7, 4.5 |
| 4 | 43.84 | 9.90 | 2.14% | 0.14% | 118.2 | 1.9 | 12.0, 8.6 |
| 5 | 42.37 | 9.19 | 2.41% | 0.00% | 117.1 | 2.7 | 6.2, 5.9 |
| median | 43.07 | 9.43 | 2.14% | 0.14% | 117.6 | 2.0 | |

The median run has `pose_to_send_ms` 10.1, `network_ms` 3.6, `decode_ms` 3.6 and `vsync_wait_ms` 16.3. The round trip was 3.6 to 3.8 ms. A run with a worse Wi-Fi (a round trip above 5 ms) or on a loaded Mac shows more frames after four periods and a mean of 50 ms and more. The display lock's share of misses rests at about 2.4%.

The block also has the headset's frame rate, the video rate, the round trip, the loss, the parity share and the display lock's numbers. The client's stats panel shows the time from the tracking sample to the end of the decode and to the display. A headset that shows no stream (Room at Passthrough, no game) reports no timing, so the block is `null` then.

## Finding the Mac

The connect panel lists the Macs that run the cranvr host. The client broadcasts a query by UDP to port 9950 (`proto/src/discovery.rs`). It sends it to `255.255.255.255`, to the broadcast address of its own /24 network and to this machine. Each host answers the sender: the Mac's name, the TCP port of the session, the start of the host key's SHA-256 fingerprint and the protocol versions that it speaks. The client lists the answers and forgets a Mac that has not answered for five seconds. A press on a Mac connects to the address that the answer came from, at the port in it. A query is longer than any answer, so a host cannot serve to amplify a spoofed query. The answer proves nothing: the handshake proves the host's key. Several hosts on one Mac share the UDP port (`SO_REUSEPORT`). `--discovery-port N` changes the port. 0 turns the answers off. A router that drops broadcasts leaves the list empty. The address field is the way around it.

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

The protocol is at version 6. The crate still speaks version 4, so a headset and a Mac that updated apart and are paired keep working. The approval needs version 5 on both sides. Version 6 adds the tracking of hands, body, face and eyes ("Hands, body, face and eyes in games"). Its messages are new variants at the end of the message enums, so a peer of version 4 or 5 skips them. A headset with the older app that the host does not know cannot wait: the host refuses it with `NotPaired` and the older `--pair CODE` window still pairs it. The host then tells the user in a `network` status line to update the app on the headset. A new headset that meets an older host that does not know it hears `NotPaired` as well and says "Update cranvr on the Mac." A host and a client whose ranges do not meet say which side is older: the older side updates. The discovery answer carries the host's range, so the connect panel shows the advice on the Mac's entry before a press. An older host does not answer the search at all.

## The USB cable

A headset on a USB cable at the Mac connects over the cable without a typed address. The cable gives the stream TCP and a steady link. The Mac looks for the adb tool (the PATH, Android Studio's SDK, Homebrew's android-platform-tools). A Mac with none shows "Add USB support" in the Settings tab, which downloads Google's platform-tools into cranvr's folder. The Mac counts a device only when `dev.cranvr.client2` or the phone build is installed on it. The app then sets up `adb reverse` for the host's port. The headset lists "This Mac by USB cable" first (`app/src/usb.rs`, `client/src/usb.rs`, `cranvr_proto::link::probe_host`). The Settings tab has a Connection choice: Automatic, Wi-Fi or USB cable. When the cable comes out, the headset connects to the same Mac over Wi-Fi by itself after about three seconds. A Wi-Fi link never moves to the cable while it runs.

## The sealed session

The protocol encrypts and authenticates everything after the version exchange. A peer of version 3 or lower is refused.

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

## The desktop panel and the sound

The client shows the Mac's desktop as its own panel and plays the Mac's sound.

The desktop stream:
- The host captures the display at its full pixel size (ScreenCaptureKit, YUV 4:2:0, up to 60 pictures a second) and encodes it with a second HEVC session (`host/src/headset/native/desktop.rs`). The units of this stream carry the stream id `DESKTOP_STREAM` (1) in the media header and share the media path with the eye stream. ScreenCaptureKit gives a picture only when the display changes, so a still desktop costs nothing.
- The client asks for the stream with `ClientMsg::Desktop` (a width and a rate). The host answers with `HostMsg::Desktop`: the picture's size in pixels, the display's size in points, the bitrate and whether the panel is visible. A game hides the panel. The host then stops the capture and sends a key frame when the panel shows again. A client that asks for no desktop, or a host without `--desktop`, leaves the desktop in the eye stream. While the panel shows, the host leaves its own screen out of the eye stream. `--no-desktop-layer` stops the panel.
- The pictures are not mirrored. Cranpose's compositor layer turns a decoder's surface upright.
- The client shows the stream in a `SpatialExternalSurface` (mono). A second `VideoDecoder` in surface mode writes into it (`client/src/desktop.rs`). The decoder has its own thread, so the eye stream's decode never waits for it. The decoder waits up to 30 ms for an input buffer: with Cranpose's default of 4 ms a busy Quest refused pictures. Each refused picture cost a key frame.
- A handle under the panel carries it. The thumbstick forward and back moves it away and near. Left and right resizes it. The client keeps the pose and the width in Cranpose's preferences service (`client/src/place.rs`).
- The controller acts as the mouse over the panel. Cranpose reports where the controller's ray meets the surface as `Enter`, `Move`, `Down`, `Up` and `Exit`. The client sends u and v on the picture, the trigger and the thumbstick as `ClientDatagram::Pointer` for each event that changes them and every 50 ms while the ray is on the panel. The host moves the Mac's mouse, clicks and scrolls (`host/src/desktop.rs`). `CRANVR_POINTER_DRY_RUN=1` logs the mouse events and posts none.
- The Mac's cursor is part of the pictures. Its position also goes out as `HostDatagram::Cursor`. `CRANVR_DESKTOP_CURSOR_IN_VIDEO=0` leaves the cursor out of the pictures.
- macOS asks for two permissions on first use. Screen Recording lets the host capture the display. Accessibility lets the controller move the mouse. Without Accessibility the panel still shows. The app asks for each missing permission when it starts. A card at the top of its window has a button for each.
- `--desktop-fps` sets how often the host captures the display (default 30). Each capture of a 6K display costs the window server about 3 ms of GPU time.

The sound:
- The host captures what this Mac plays, from every app but the host (`host/src/game_audio.rs`). It sends the sound as PCM: 48 kHz, 16 bits, stereo, in datagrams of 240 frames (5 ms, 960 bytes). That is 1.54 Mbps on top of a video stream of 40 to 100 Mbps. PCM adds no encode or decode time and needs no codec on the Quest. A datagram carries the host's clock for its first sample, so the client can tell how old the sound is.
- The capture takes the sound before the Mac's volume, so the Mac can stay muted while the headset plays it. It needs Screen Recording. "Game sound in the headset" in the settings turns it off. `--no-game-audio` does the same. The headset's microphone does not reach the Mac.
- The client plays it through Cranpose's audio backend (`client/src/audio.rs`). The renderer runs on the platform's output thread (AAudio, low-latency mode) and takes stereo frames from a ring. A jitter buffer between the network thread and the renderer plays after it holds 30 ms, keeps its fill near the target by changing the playback rate by at most 0.5%, drops its oldest sound when the fill passes 90 ms and leaves a gap of silence for a datagram that never came.
- The client logs the buffer's fill, the age of the sound when it was queued, the lost, late and dropped datagrams and the underruns every second. The stats panel shows them.
- The sound reaches the headset's buffer about 45 ms after the Mac plays it. A rhythm game such as Beat Saber has an audio latency setting: set it once with its calibration.

## Controllers in games

The host feeds BasaltVR's runtime (`host/src/runtime_link.rs`) from the client's `Tracking` and `Input` messages. For each tracking sample it sends the head pose, the grip pose and the aim pose of each controller that the headset tracks with the velocities of both. It also sends the mask of tracked controllers. It sends every change of a button, a touch, the trigger, the grip and the thumbstick with the path ids of the Touch profile (`common/src/inputs.rs`). The runtime's haptic requests go back to the client as `Haptics` datagrams.

Each controller has two poses. The grip pose is the place of the hand that holds the controller. The aim pose is where the controller points. The client sends both as the headset reports them, with no offset. The host adds only the floor's height. It hands both poses to the runtime in one control message for each hand, `CONTROLLER_MOTION_AIM` (cranvr's runtime patch 0004, in `third_party/basaltvr/patches/runtime`). The runtime gives a game the aim pose for an action space on `/input/aim/pose` and the grip pose for one on `/input/grip/pose`. A hand that sends no aim pose has its aim space at the grip pose.

A runtime without patch 0004 closes the connection at a message that it does not know. The patched runtime therefore writes `runtime-features.txt` next to the control file when its session starts. The host sends the new message only to a runtime that wrote the file. An older runtime gets the grip pose alone. The host then logs a warning. The runtime's control protocol keeps its version 9.

The host's stats line shows what reached the game in each 5 seconds: `Game input: 601 left and 601 right controller poses (tracked mask 0b11), 21 button and axis values, 2 haptic pulses from the game`. `scripts/e2e.sh --game` runs a fake game (`cranvr-fake-game`) that checks the input of the probe's script: both hands tracked, each hand with the grip pose and the aim pose that the probe scripted (`probe/src/hands.rs`), the right hand moving, a pull of the right trigger, a press of A, the right stick and the left grip. The fake game reads the runtime's control messages. It does not call `xrLocateSpace`, so this run checks the way from the probe through the host to the runtime's control channel.

## Hands, body, face and eyes in games

A Windows VR game under cranvr's Wine stack gets the user's hands, body, face and eyes through the OpenXR extensions that a PC headset offers. The runtime lists an extension only while the connected headset offers its feature:

| Feature | Extensions | Data |
| --- | --- | --- |
| hands | `XR_EXT_hand_tracking` | 26 joints for each hand, with the joints' velocities |
| body | `XR_FB_body_tracking` | 70 joints, the default set, with a T-pose skeleton |
| full body | `XR_META_body_tracking_full_body` | 84 joints with the legs |
| face | `XR_FB_face_tracking2` | 70 blend shape weights and two confidences |
| eyes | `XR_EXT_eye_gaze_interaction` | a gaze pose on `/user/eyes_ext/input/gaze_ext/pose` |

The way from the headset to the game:

1. After its welcome, a client of protocol version 6 sends `ClientMsg::TrackingOffer` with the features that its headset has and its user allowed. The host writes them as words to `tracking-features.txt` next to the runtime's control file (`hands body full_body face eyes`, with `face_visual` or `face_audio` after `face`) and removes the file when the headset leaves. A game lists its instance extensions before its session exists, so the runtime reads the file at that moment. A game that starts before the headset connects sees no tracking extension.
2. The patched runtime (patch 0005) tells the host which features its game uses (`TRACKING_REQUEST`, message 19): a hand tracker, a body tracker or a face tracker, or an eye gaze action that the game attached. The host asks the headset for exactly these (`HostMsg::TrackingWish`) and asks again when the set changes. A feature that no game uses costs no traffic.
3. For each headset frame the client sends one datagram for each wished feature (`Hands`, `Body`, `Face`, `Eyes`, in `proto/src/tracking.rs`). Each carries the sample time and display time of the frame's `Tracking` message. Joints travel as poses relative to a root pose with full precision and a velocity. The relative part is quantized: a position to 0.1 mm and an orientation to its three smaller components in 16 bits, so the error stays below 0.1 mm and 0.005 degrees. The largest datagram, the 84 joint body, takes 1197 of the 1400 bytes of a packet. A face weight takes one byte.
4. The host lowers the poses by the headset's floor, as for the head and the controllers, and sends them to the runtime (messages 20 to 23, documented in `basalt_control_protocol.h`). The runtime sends the joints in the base space that the game names. A hand and a body move forward by their velocity for the time from the sample's arrival to the game's locate time, at most 50 ms. The runtime has no map from the headset's clock to the game's `XrTime`, so the arrival time stands in for the sample's time. The headset has already predicted the sample to its own display time.

A runtime without patch 0005 closes the connection at a message that it does not know. The patched runtime therefore writes the word `tracking` to `runtime-features.txt`, and the host sends messages 20 to 23 only to a runtime that wrote it. Without the word the host still makes controllers from hands. An older host never writes `tracking-features.txt`, so a patched runtime then lists no tracking extension. A host or a client of protocol version 5 or older skips the new messages and takes no harm.

### Where the face comes from

`XR_FB_face_tracking2` lets a game choose the data source of its face: the cameras (visual) or the microphone (audio). A Quest 3 has no camera that sees the mouth, so its face is the audio one. The offer carries the source, and runtime patch 0006 reports it to the game by the rules of the extension:

- `xrGetSystemProperties` sets `supportsVisualFaceTracking` and `supportsAudioFaceTracking` from the offer. A Quest 3 supports audio only. A face of an unknown source reports both. An older headset app offers such a face, and so does a headset that has not yet given a valid sample.
- `xrCreateFaceTracker2FB` takes `requestedDataSources` by the extension's rules. An empty list accepts any source. A duplicate, a value that is no source and a missing list for a counted one get `XR_ERROR_VALIDATION_FAILURE`. While no headset offers a face, the runtime makes the tracker and reads no face, so that a game survives a reconnect of the headset.
- `xrGetFaceExpressionWeights2FB` sets `dataSource` from the newest sample, which says its own source.

One case deviates from the extension on purpose. The extension calls the list the sources that the game accepts. It says that a runtime must return `XR_ERROR_FEATURE_UNSUPPORTED` when it believes that it will never serve the list, and a list without a source of the headset's face is such a list. This runtime makes the tracker instead and serves the headset's face. One condition in `createFaceTracker2` (patch 0006) restores the error. A game that lists the cameras only still gets the face of a Quest 3, and `dataSource` tells it that the face comes from the microphone. Every game that creates a face tracker therefore reads the headset's face.

### Controllers made of hands

A game without hand code binds the Oculus Touch controller profile and knows nothing of hands. While the headset tracks a hand and no controller of that hand, the host makes the controller from the hand (`host/src/hand_controller.rs`):

- The grip pose is the palm's pose. The aim pose starts between the thumb tip and the index tip and points from an estimated shoulder through that point, with no roll.
- Pinching thumb and index finger pulls the trigger: the value runs from 0 at 60 mm between the tips to 1 at 18 mm, and the runtime reads a trigger from 0.5 as a click. Closing middle, ring and little finger presses the squeeze button.
- The host does this with the existing controller messages, so it works with a runtime of any version.
- A game that creates a hand tracker has hand code of its own and gets no controller made of a hand. The host also waits for 3 seconds after the game connected before it makes any, because a game with hand code creates its tracker soon after it starts. Neither follows the game's name. `--no-hand-controllers` turns the controllers off.
- While no controller is tracked for 0.3 s, the host asks the headset for hands. It stops asking when both controllers have been back for 3 s.

`XR_EXT_hand_interaction` is not offered. A game that binds only its interaction profile finds no input. The grip and aim poses of the Touch profile cover a game that has no hand code.

### Tests

`cargo test` covers the messages (`proto/tests/tracking.rs`: quantization, packet sizes, what an older peer decodes), the host's link to the runtime (`host/src/tests/tracking.rs`) and the conversion of hands to controllers (`host/src/hand_controller.rs`). Three runs of `scripts/e2e.sh` cover the whole way. They use the probe's script of `probe/src/tracked.rs`, in which the head, the hands, the body and the face stand still:

```bash
# The fake game asks for the features. It checks every joint, weight and the gaze against the script.
# --face-source visual|audio|unknown sets where the probe's face comes from (default audio, a Quest 3's).
scripts/e2e.sh --native-port 9961 --tracking
# A game without hand code: the probe tracks hands and no controller.
scripts/e2e.sh --native-port 9961 --hand-controllers
# The real runtime: tracking_smoke.exe reads everything through OpenXR in Wine.
CRANVR_HOME=/path/to/a/test/stack scripts/e2e.sh --native-port 9961 --tracking-wine
CRANVR_HOME=/path/to/a/test/stack scripts/e2e.sh --native-port 9961 --tracking-wine --face-source visual
```

`--tracking-wine` needs a stack built from the runtime under test (`CRANVR_HOME=... scripts/setup-wine.sh third_party/basaltvr/src`). `tracking_smoke.exe` checks that the runtime lists the extensions, answers the system properties, refuses a joint set that it lacks, returns a skeleton, and gives the 26 hand joints of both hands, 70 and 84 body joints, 70 face weights and the gaze within 0.5 mm and 0.1 degrees of the script. It also checks the joints in the view space and the joints' velocities. For the face it checks the support structure against `--face-source`, the answer of the creation to each list of sources and the `dataSource` of each sample. `--live-face` accepts any weights, so that the program can read the face of a real headset. The run checks that the host asked the headset for tracking only after the game created its trackers and that it asked for none after the game left.

## Games

Games need Wine with cranvr's DXMT fork and BasaltVR's OpenXR runtime. The app's Play tab has one button, "Set up games". It runs the steps that the Mac still lacks, in this order, with a line of progress for each step:

1. Rosetta 2, which runs Wine's Intel code. macOS asks for an administrator's password. The Play tab says so before the button is pressed.
2. Game support: the app downloads `cranvr-game-support-macos.tar.xz` (about 240 MB) from the public release at https://github.com/samoylenkodmitry/cranvr-game-support, checks its SHA-256, unpacks it and installs it with `setup-wine.sh`. It takes about 1.2 GB on disk. `scripts/install-game-support.sh [ARCHIVE|URL]` does the same without the app.
3. Steam for Windows. At the end the Steam window opens. The user signs in there once.

A failed step keeps its message on screen with a Retry button, which runs the steps that are not done. Games on another disk need no setting in cranvr: in the Steam window, Settings, Storage adds a folder on that disk. The Play tab lists the games of every Steam library folder.

The game support has a version, a date such as `2026.11.19`, in `scripts/game-support-version.txt`. `package-game-support.sh` puts it into the archive as `VERSION`. `setup-wine.sh` stamps it into `wine/game-support-version.txt` in the config folder after a good install. When the stamp is older than the app's version, or missing, the Play tab shows "Update game support". After a change of a runtime or DXMT patch: set a new date in the version file, run `build-basaltvr.sh` and `package-game-support.sh`. Publish the archive and its `.sha256` as a release of the public repository, tagged `v` plus the date.

To build game support from source instead (about an hour, mostly LLVM):

```bash
xcodebuild -downloadComponent MetalToolchain
softwareupdate --install-rosetta
brew install cmake meson ninja mingw-w64
scripts/build-basaltvr.sh
scripts/setup-wine.sh third_party/basaltvr/src
```

`build-basaltvr.sh` clones BasaltVR at a pinned commit into `third_party/basaltvr/src`, applies cranvr's patches and builds the OpenXR runtime and the DXMT fork. `setup-wine.sh` puts Wine 11.10 with the DXMT fork in `~/Library/Application Support/cranvr/wine`, registers the runtime in the Wine prefix and checks it through the OpenXR loader. `scripts/package-game-support.sh` packs a built tree into the archive and its `.sha256` in `target/game-support`.

Start the host first: it writes `runtime-control.txt` into its config dir. Then start a game from the window or with the script:

```bash
scripts/run-game.sh 'C:\basalt\runtime\d3d11_runtime_smoke.exe' --render-frames 7200 --visual
```

The smoke program draws a green marker at the left controller and a pale grid. A squeeze of the left grip grows the marker and sends a haptic pulse.

### Steam games

Steam runs in the prefix with BasaltVR's web helper shim (`scripts/setup-steam.sh`):

1. In the window's Play tab, press "Open Steam" (after "Set up games" it has opened Steam already). Log in. Install the game.
2. The tab lists the installed games. Press "Play" next to one: the window starts it by its .exe with the host's environment.
3. Without the window, start Steam with `scripts/run-game.sh --steam`. While Steam runs, start the game by its .exe: `scripts/run-game.sh 'C:\Program Files (x86)\Steam\steamapps\common\Beat Saber\Beat Saber.exe'`.

A rebuild of BasaltVR gives the web helper shim new bytes. The app then reports that Steam needs its setup step. "Set up games" then runs `setup-steam.sh` again. Do not use `steam.exe -applaunch`: a game that Steam starts gets Steam's environment, not the one with the runtime's control file. For a game under `steamapps\common` the script and the window find the Steam app ID in Steam's `appmanifest` files and set `SteamAppId`, which the game's Steam check needs.

The runtime speaks OpenXR with D3D11. Games that use OpenVR only need OpenComposite (see BasaltVR's `docs/BOTTLES.md`). The app looks for a game's program three folders deep, preferring a 64-bit folder.

A game that the app or `scripts/run-game.sh` starts draws nothing into its Mac window: `DXMT_SKIP_PRESENT=1` (cranvr's DXMT patch 0008) sends each frame's GPU work without the present but tells the game that the present worked. `CRANVR_MAC_MIRROR=1` for the script draws the window again.

### What the host and the app do for a game

Neither the host nor the app knows a game by name. A game's quirk is a launch option (`app/src/launch.rs`, shown in the Play tab), set by the user or by cranvr with a reason shown in the window.

- **Render size.** `--game-scale F` (default 0.7, "Game render size" in the Settings tab) makes a game render each eye at this share of the stream's eye size. The host keeps a render size per game. A game that stays below 95% of the headset's rate for 10 of the 12 stats windows in its first minute after loading gets a smaller share at its next start. The step aims 10% under the rate, with a floor of 40%. When a smaller size did not raise a game's rate by 5%, the host stops lowering it. The runtime names the game in `game-program.txt` and reads the shares from `game-sizes.txt` in the config folder. The Settings tab lists the learned sizes and forgets them. A game that makes fewer frames than the headset's rate adds 10 to 18 ms of latency. The host warns in the log 20 seconds after the game connects when the game stays below 95% of the rate.
- **Anti-aliasing.** Games under Wine use at most 2x anti-aliasing by default ("Game anti-aliasing" in the Settings tab, `CRANVR_MAX_MSAA` for `scripts/run-game.sh`, 0 for no limit). cranvr's DXMT patch then reports 4x and 8x as unsupported, so a game that checks picks 2x. It applies when a game starts. A game started from the Steam window gets it after Steam restarts.
- **Panel pixels.** While a game runs, a display in a scaled mode shows its panel's own pixels ("Panel pixels while a game runs" in the Settings tab, on by default, `--keep-display-modes` turns it off). A scaled mode, such as 3008 x 1692 points on a 3840 x 2160 TV, makes macOS draw every change at 6016 x 3384 pixels and shrink it. A helper process (`cranvr-host --unscale-displays`) sets each such display to the panel's pixels with at least as many points, so no window moves, for itself only. When the game disconnects, the host ends the helper. macOS then gives each display its mode back.
- **A black right eye.** A game that renders both eyes in one pass into a swapchain with two slices can leave the second slice empty under DXMT. The host finds the symptom (`host/src/eye_check.rs`): about once a second it reads a grid of 16 x 16 pixels from each eye of the game's newest frame. When the left eye has content (at least 8 lit samples) and no sample of the right eye is lit for 4 checks in a row, it sends one `black_eye` status line for the game run. The app answers in `app/src/black_eye.rs`: it turns on the launch option "Each eye in its own pass" for that program, restarts the game and shows a notice in the window and in the console. The option sets `BASALT_MAX_ARRAY_SIZE=1` (runtime patch 0003), so the runtime rejects two-slice swapchains and the game draws each eye into a swapchain of its own. A dark screen in both eyes, such as a loading screen, never counts. A flicker between states never counts either.

## A virtual display for the headset

By default the panel in VR shows the Mac's main display as it is. The Desktop tab can add a display instead: one more monitor of this Mac that only the headset shows (`host/src/virtual_display.rs`). It is 1920 x 1080 points with HiDPI (3840 x 2160 pixels) at 60 Hz, or 2560 x 1440. macOS has no public call for this. The host uses CoreGraphics' CGVirtualDisplay classes through the Objective-C runtime. The host makes the display at start. The display goes away with the host.

1. Start cranvr. The virtual display appears to the right of your displays, as an extra screen. Your main display keeps its place and resolution. No window moves.
2. Connect the headset. The panel in VR shows the virtual display.
3. Drag a window onto the virtual display with the Mac's mouse, or point at it with the right controller. Type with the Mac's keyboard.

cranvr moves no window by itself. Two switches in the Desktop tab change that. Both are off by default:
- "Main display while the headset is on": at the first pose of the stream the virtual display becomes the main display. macOS then moves the menu bar, the Dock and the windows of the main display onto it. New windows open there too. When the headset leaves, every window goes back to its place and size. The change applies to the host process only (`kCGConfigureForAppOnly`), so macOS also undoes it when the host ends. `cranvr-host --desktop --virtual-display 1920x1080 --vr-main-display` does the same without the window.
- "Bring the cranvr window into VR": the cranvr window moves onto the virtual display while the headset streams. It moves back afterwards.

A cranvr window that sits on the virtual display when no headset streams moves to the main display: nobody would see it there.

## Test without a headset

```bash
cargo test --release
cargo test --release -p cranvr-app -- --ignored
cargo test --manifest-path client/Cargo.toml
./target/release/cranvr-host --config-dir /tmp/cranvr-test &
./target/release/cranvr-probe --seconds 10
./target/release/cranvr-probe --seconds 10 --tcp            # media over TCP, as over USB
./target/release/cranvr-probe --seconds 10 --drop-percent 3 # lets the parity recover packets
```

The probe is a client of the protocol on this Mac. It connects to port 9950, sends a head that turns 20 degrees a second and scripted controllers once per refresh, restores lost packets with the parity, decodes with VideoToolbox and checks the red and blue eye badges of the host's scene. It simulates a headset's display and reports the frame timing and the loss to the host. It exits with code 0 when at least half of the expected frames decode with the right eye colors. A client on this Mac needs no pairing. The ignored app test starts the host the way the window does and connects the probe. The client's ignored tests drive a real host with the client's connection code in a flat window.

The game path without Wine:

```bash
./target/release/cranvr-probe --game --seconds 14 &
./target/release/cranvr-fake-game --control /tmp/cranvr-test/runtime-control.txt --seconds 10
```

`cranvr-fake-game` speaks the runtime's protocol and shares IOSurfaces like the Wine runtime. Its eye badges have swapped colors, so `--game` tells its frames from the test scene.

## End-to-end test

`scripts/e2e.sh` runs the host, a client and an optional game, then writes a JSON report. The report holds every 5-second stats line of the host and the median and worst of each number: the frame rate, the render and encode times and the latency block (see "Latency numbers"). The script exits with 0 when the host and the client both reach 90 percent of the target rate after the first stats line. With the probe it must also show 95 percent of the frames that reached its decoder.

```bash
scripts/e2e.sh --fps 120 --game
scripts/e2e.sh --native-tcp --native-drop 3
scripts/e2e.sh --quest 2G0YC5ZH2000XX --native-port 9961 --seconds 30
scripts/e2e.sh --seconds 30 --wine 'C:\basalt\runtime\d3d11_runtime_smoke.exe' \
    --wine-arg --render-frames --wine-arg 14400 --wine-arg --visual
```

`--wine` runs a Windows OpenXR program in cranvr's Wine stack without sound. The probe then counts every frame of the right size, since a real program draws no eye badges. `--desktop` starts the host with its desktop panel. The probe then asks for the desktop stream, plays a pointer script (the host only logs its mouse events, as `CRANVR_POINTER_DRY_RUN=1` does) and counts the Mac's sound. It plays no sound: it measures what arrives. The run fails when the probe gets the Mac's sound for less than 80 percent of the stream time.

`--quest` starts cranvr's client on a Quest attached by USB (install it with `scripts/build-client.sh --install SERIAL`). adb keeps its display on without a face in the headset. The script pauses the guardian for the run. The Quest and the Mac must share a network. The host runs with `--trust-all` on `--native-port` (default 9950). A headset keeps one host key for each address and port. Pick a port that the headset has not used with another host at this Mac's address, such as 9961. The app gets this Mac's address and the room `scene` through its launch extras. The run keeps the host's key in `target/e2e/host-identity.txt`, because a headset refuses a known address with a new key. Before a Quest run, the harness reads the device's network over adb. It warns when a VPN on the device routes this Mac's address, when the device uses 2.4 GHz Wi-Fi and when its Wi-Fi link runs below 400 Mbps. Two runs with the same setting differ by up to 5 ms in total, so compare two settings with several pairs and keep every report. Check `uptime` before a run: other programs on the Mac lower the rates.

A phone or tablet runs the phone build of the client against a host on this Mac. Use `adb reverse tcp:9962 tcp:9962` and a host with `--native-port 9962`, or the Wi-Fi address of the Mac.

### Measure a game with a Quest on the desk

`scripts/perf/` holds the tools behind the game measurements.

```bash
scripts/perf/game-run.sh aa2 20
CRANVR_MAX_MSAA=0 scripts/perf/game-run.sh aa-game 20
HZ=240 scripts/perf/game-run.sh uncapped 20
scripts/perf/metal-trace.sh bs
python3 scripts/perf/gpu-by-kind.py target/perf/bs.xml "Beat Saber"
```

`game-run.sh` needs the cranvr app running and a headset that is paired with it. It wakes the headset without a wearer and starts cranvr's client and the game without sound. `CRANVR_HOST=ADDRESS:PORT` gives the client the Mac's address. Without it the client uses the address of its last connection. It prints one summary line of the rates and the latency stages. Then it gives the headset its guardian back. The proximity sensor stays closed, so the headset stays awake. `metal-trace.sh` records 5 seconds of GPU work of every process on the Mac while a game runs. `gpu-by-pass.py` sums that work per process and per pass. `gpu-by-kind.py` sums it per kind of pass. `display-modes.swift` lists the modes of the external display or holds one for its own run.

## Other settings of the host

- The encoder overlaps up to two frames. A data rate limit over each second keeps the stream at or under the bitrate target. `--strict-rate-control` switches to VideoToolbox's low-latency rate control: even frame sizes, one frame at a time, slower per frame on an M5 (6.1 ms against 3.6 to 5.0).
- `--no-foveation` encodes every pixel at full density. It needs a smaller eye size.
- `--pointer-pitch DEG` tilts the pointing ray down from the controller's handle (default -40). It applies to a client that sends no aim pose. A client's aim pose gives the ray as it is.
- A headset with Tailscale that accepts a subnet route for its own network sends its UDP to the Mac through the subnet router, which changes the sender address. The stream may then not arrive. The handshake still works over TCP, so the headset connects, gets no video and leaves after about 5 seconds, again and again. The host reports it after two such connections in a row. The fix is on the headset: exclude the cranvr app from the VPN, or stop using the subnet route there. The host also warns when this Mac reaches the headset through a VPN tunnel of its own. `--tcp` sends the media over TCP to every headset.

## Network exposure

- The host listens on TCP 9950 (`--native-port`) on all interfaces. A client that the host does not know waits for the user's Allow and gets no stream before. Everything after the handshake is sealed.
- The host answers discovery queries on UDP 9950 (`--discovery-port`) on all interfaces.
- The game link listens on 127.0.0.1 only. A game must show the token from `runtime-control.txt` (mode 0600).

Use pairing for real use. `--trust-all` accepts any client on the network and is meant for tests.

## Not tested yet

- Hands, body, face and eyes from a worn headset. The tests run with the probe's script and the client's own tests. A real hand in view, a real face and the permission prompts of a user who answers them have not been tried. The face source of a worn Quest 3 and the face of a real Android XR headset have not been seen either.
- The cranvr client in a worn headset: the controllers in a game, the carry of the desktop panel by a ray, a recenter and the cable from a Quest.
- Beat Saber during a song. The game's frame rate and latency were measured on its first screen with the headset on a desk.
- A game that hides the desktop together with a recenter.
- A host that restarts during a stream with several clients one after the other.
- The controller as a mouse on the virtual display in a real headset.
- Sound with the Mac muted in a worn headset.

## Next steps

`docs/plan.md` lists the work that the protocol allows. `docs/cranpose-xr.md` covers Cranpose apps for VR stores.

## License

MIT. The foveation math follows ALVR v20.14.1 (MIT, `third_party/licenses/ALVR-MIT.txt`). BasaltVR is MIT.
