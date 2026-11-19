# First run on a Quest: Steam Beat Saber at 120 Hz

This list covers a first run with cranvr's Cranpose client on a Quest. The client ran on a Quest 3 on a desk with the host's test scene and with Beat Saber's first screen. Nobody has played a song in a worn headset yet. The stock ALVR client works too: see "With the stock ALVR client" below.

## On the Quest

1. Turn on developer mode for the Quest in the Meta Horizon app on your phone. Connect the Quest to the Mac by USB. Allow USB debugging in the headset. The cable is optional for the stream: see "The USB cable" below.
2. In the cranvr folder, run `scripts/build-client.sh --install`. Add the Quest's serial from `adb devices` when another device is attached.
3. Connect the Quest to the same Wi-Fi as the Mac, on 5 GHz or 6 GHz.

## On the Mac

1. Open `/Applications/cranvr.app`. The host starts by itself.
2. In the Settings tab, under "Headset app", pick "cranvr client". The app then starts the host with `--protocol native`. The Headset card says "Open cranvr on the headset. It finds this Mac." Below it, a line shows this Mac's address with the port, such as `192.168.1.20:9950`. You need the address only when the headset finds nothing.
3. macOS asks for Screen Recording and Accessibility when cranvr starts. Until both are allowed, a card at the top of the window shows an "Allow" button for each. The game sound needs Screen Recording. Accessibility is only for the controller as a mouse in the desktop view.
4. In the Play tab, press "Open Steam" and sign in. Steam may ask for a Steam Guard code.
5. Install Beat Saber in the Steam window. Start the download early.
6. Turn the Mac's volume down. The headset gets the sound at full level anyway.

## Connect

1. In the headset, open the app library and pick "cranvr client" under Unknown Sources. macOS may ask whether cranvr may use the local network: allow it. Without it the host sees no headset and answers no search.
2. The connect panel lists the Macs that run cranvr on the same network, each as a button. Press your Mac. The list needs a few seconds. It shows the name that the Mac has in System Settings, such as "Dmitry's MacBook Pro".
3. A headset that the Mac does not know yet shows a code of six digits and "Allow it on the Mac". The cranvr window on the Mac comes to the front with a card: "Meta Quest 3 wants to connect. It shows the code 482 913." Compare the code in the card with the code in the headset. Press Allow only if the two codes are the same. A different code means that another device sits between the headset and the Mac. Press Deny then. The headset connects at once after Allow. The Mac waits three minutes for your answer.
4. A later connection needs no answer. Press the Mac again. The panel shows the state of the link.
5. The Headset card should read "at 120 Hz". If it reads another rate, pick 120 Hz in the Settings tab. Below it, a line shows the frame rate and the time from motion to display. The stats panel in the headset shows the same numbers.

## The USB cable

A headset that sits on a USB cable at the Mac connects over the cable without a typed address. The cable gives the stream TCP and a steady link, and it needs no Wi-Fi. This is how it goes:

1. Developer mode must be on for the headset (see "On the Quest"). Plug the headset into the Mac. The headset asks to allow USB debugging: allow it. Until then the Headset card in the Mac window says "Put on the headset and allow USB debugging."
2. The Mac looks for the adb tool of Android (the PATH, Android Studio's SDK, Homebrew's android-platform-tools). A Mac with none shows the button "Add USB support" in the Settings tab, under Connection. The button downloads Google's Android platform-tools for macOS (about 16 MB) from dl.google.com into cranvr's folder. A Mac that has a headset on the cable but no cranvr app on it does nothing: the Mac counts a device only when `dev.cranvr.client2` is installed on it.
3. The Mac then sets up the link by itself (`adb reverse` for the host's port). The Headset card says "The headset is on the USB cable. It connects over the cable."
4. The connect panel in the headset lists "This Mac by USB cable" first. Press it. The cable needs no code. The Wi-Fi line of the same Mac does not show: each Mac has one line, and the cable wins. A typed address of the same Mac takes the cable too. The Mac window then says "Connected by USB cable". Over Wi-Fi it says "Connected over Wi-Fi".

The Settings tab has a Connection choice. "Automatic" uses the cable while the headset is plugged in and Wi-Fi otherwise. "Wi-Fi" takes the link over the cable away, so the headset sees no cable and uses Wi-Fi. "USB cable" keeps the link, and the Headset card says when the headset is not on the cable ("Plug the headset into this Mac with a USB cable.") or when a headset connects over Wi-Fi anyway. A change counts from the next connection: a running stream keeps its path.

When the cable comes out, the headset connects to the same Mac over Wi-Fi by itself, after about three seconds. It finds the Mac by its key's fingerprint. A headset that has only used the cable has not paired with the Mac. Its first Wi-Fi connection shows a code, and the Mac asks you to allow the headset, as in "Connect" above. A Wi-Fi link never moves to the cable while it runs. When it ends, the next connection uses the cable if the headset is plugged in.

An adb tool stops a running adb server of another version. The downloaded copy of adb could therefore stop the server of Android Studio. The Mac uses the downloaded copy only when no adb server of another version runs. The window says "Another adb tool runs on this Mac. Close it, then cranvr can use the cable." otherwise.

The address entry stays for a Mac on another network, which the search does not reach. Without the Mac's setup, `adb reverse tcp:9950 tcp:9950` and the address `127.0.0.1:9950` still work. The client keeps the last address that you pressed or typed. A start with the extras `--es host ADDRESS --ez connect true` connects to a Mac without a press (`adb shell am start ...`). The extra `--ei usb_port PORT` moves the port that the headset asks on the cable, for a test host on another port.

The Mac and the headset update apart. When the two apps speak different protocol versions, both say which one to update: "Update cranvr on the Mac." or "Update the cranvr app on the headset." A Mac with a cranvr older than the headset's does not appear in the list, because it does not answer the search. Press "Enter an address" and type its address to see the advice. A Mac with an older cranvr that never paired a headset listens on its own address only. The headset then says that the Mac refused the connection, and the text names the update of cranvr on the Mac as a cause. A headset that has paired with the Mac keeps connecting while both apps speak a common version. A headset with the older app that the Mac does not know cannot ask: the Mac shows a note that tells you to update the cranvr app on the headset.

## Work from the headset

The headset shows the Mac's main display in a panel as a second video stream at the display's pixel size, so its text is sharp. The Mac's cursor is part of the picture.

- To add a display that only the headset shows, pick "Add a display" in the Desktop tab. It sits to the right of the TV. No window moves by itself: drag windows onto it with the Mac's mouse.
- Point at a window with a controller to move the mouse. Pull the trigger to click. Push the thumbstick to scroll. Type on the Mac's keyboard.
- To move the panel, point at the handle under it, hold the trigger and move the controller. Push the thumbstick forward or back to move the panel away or near. Push it left or right to resize the panel. The client keeps the panel's place for the next start.
- Hold the left controller's menu button for a moment to show or hide the console. It has a button for each installed game and buttons for the main settings. A row at its bottom has Close, Disconnect and Exit.
- With an added display, "Main display while the headset is on" moves everything into VR while the headset streams. The menu bar, the Dock and your windows then move from the TV into VR. They go back when the stream ends. "Bring the cranvr window into VR" moves only the cranvr window. Both are off by default.

## Play

1. Press "Play" next to Beat Saber in the cranvr window on the Mac, then put on the headset. The console in the headset has a button for each game as well.
2. In Beat Saber, open Settings, then Audio. Run the audio latency calibration once. The sound reaches the headset's buffer about 45 ms after the Mac plays it.

## When something goes wrong

- The headset does not list "This Mac by USB cable": the Mac window must say "The headset is on the USB cable." (Headset card, no headset connected). It does not when the Connection choice in the Settings tab is Wi-Fi, when the headset has no developer mode, when the cranvr app is not installed on the headset, when this Mac has no adb tool (press "Add USB support") or when the headset waits for you to allow USB debugging. The Log tab lines that start with "USB:" show what the Mac did. A cable that the headset cannot reach shows no line at all, because the headset asks the Mac itself and a closed link gives no answer.

- The panel in the headset shows a code and "Allow it on the Mac": look at the cranvr window on the Mac and press Allow. The headset says "Nobody answered on the Mac" after three minutes. Press the Mac again to ask once more. It says "The Mac denied this headset" after a Deny.
- The list in the headset stays empty: check that the Mac and the headset use the same Wi-Fi. A router with client isolation blocks the search. Check that cranvr may use the local network in System Settings, under Privacy & Security. Check that the host runs: the cranvr window says "Host running". Use "Enter an address" as the way around.
- The panel in the headset says that the host does not have the key that the headset paired with. This happens after a new install of the host. Press "Forget host key" in the connect panel, then connect again.
- The headset does not connect: check the Wi-Fi. Check that cranvr may use the local network in System Settings, under Privacy & Security. Then read the Log tab. A notice in orange names network problems.
- No sound in the headset: the headset card says why. Check Screen Recording in the Desktop tab. If it reads "not allowed" while System Settings shows cranvr as allowed, remove cranvr from that list, allow it again, then quit and reopen cranvr. The host tries the capture once per start: turn "Game sound in the headset" off and on in Settings to try again.
- Frames drop at 120 Hz, or the Log tab says that the game is below the headset's rate: keep "Game anti-aliasing" at "At most 2x" in the Settings tab. On the base M5 Beat Saber's first screen ran at 91 frames a second with 4x and at 119 with 2x. A game that stays slow for a minute renders smaller at its next start by itself. A smaller Game render size in the Settings tab lowers every game. Keep "Panel pixels while a game runs" on: a TV in a scaled mode costs the Mac's desktop far more GPU time. Turning the Desktop view off also saves GPU time.
- Beat Saber starts but stays black. Its process uses no CPU. Wine's desktop process may hang. This happened once after a day of display tests. Quit Steam and the game. Run `WINEPREFIX="$HOME/Library/Application Support/cranvr/wine/prefix" "$HOME/Library/Application Support/cranvr/wine/engine/bin/wineserver" -k`. Then start Steam again. The cause is not known.
- Beat Saber does not start: `~/Library/Application Support/cranvr/game.log` and `game.err.log` hold its output.
- Send me the line under the headset card and the end of the Log tab.

## With the stock ALVR client

The host serves the stock ALVR client with `--protocol alvr`, its default. In the Settings tab, "Headset app" says "ALVR client" for this mode.

1. Install ALVR from the Meta store. The store has 20.14.0. cranvr works with every 20.x version. A v21 nightly build does not connect. The cranvr window then shows a notice.
2. Connect the Quest to the same Wi-Fi as the Mac, on 5 GHz or 6 GHz.
3. If Tailscale runs on the Quest, exclude the ALVR app from it in the Tailscale app. Without that, cranvr's "Network: Auto" setting moves the stream to TCP after about 10 seconds. TCP adds about 2 ms.
4. Open ALVR on the Quest. The headset shows up in the cranvr window with a "Pair with ..." button. Press it. On the first connection the Quest shows "The streamer is restarting" once and reconnects by itself.

The Network choice and the Low latency switch in the Settings tab count for this client only. If the headset connects but no picture comes after about 20 seconds, pick TCP under Network. The automatic switch to TCP has not run in the window yet. If the game feels late, try the Low latency switch. It saved about 4 ms in a test on a desk. If the picture then stutters, turn it off. ALVR buffers about 50 ms of sound.

## What the tests covered

- Beat Saber's first screen through the Wine stack on a Quest 3 on a desk, with the cranvr client and the native protocol: 117.8 frames a second shown.
- The stream at 120 Hz to the Galaxy Tab S7+ through ALVR's own client core and its MediaCodec decoder: 119.5 to 120 frames a second, 34 ms from motion to display, 97% of the frames shown.
- A real Direct3D 11 OpenXR program under Wine at 120 Hz to the Tab: 119.7 frames a second.
- The game sound from the Mac to the client: 20.5 seconds of sound in a 20-second run, over USB with TCP and over Wi-Fi with UDP.
- The notice for an ALVR app of another version: a discovery packet with another protocol ID produced it.
- The way of a new headset to the Mac, with a real host process on port 9962 and the client's connection code in a flat window on the Mac (`cargo test --manifest-path client/Cargo.toml -- --ignored a_new_headset`): the search found the host by its network address, the panel showed the code 172 624 and the host's status line listed the same code, Allow on the host's input paired the headset, and the client received 55 to 64 frames a second at 60 to 72 Hz with no loss. A second start of the client connected without a question. The phone build of the client on a Galaxy Tab S7+ over Wi-Fi to a test host on port 9962 did the same: the panel listed "s's MacBook Pro" from the UDP broadcast, a press showed the code 301 028 and the host's status line listed the same code, Allow on the host's input started the stream (the test scene at 60 Hz: the host sent 59.4 frames a second, the Tab showed 41.9 a second in a DeX window, 89 ms from motion to display), and a second start of the app connected without a question. Unit and integration tests cover the messages of the search, the code, a middle machine (two different codes), a host that lies about its commitment, a refusal, a timeout, a headset that leaves, a second headset, a headset with the older protocol and a version mismatch.

- The USB cable with a Galaxy Tab S7+ on the phone build of the client and a test app on port 9961, on this Mac with its real adb: the app set `adb reverse tcp:9961 tcp:9961` on the Tab and on no other device (an attached phone without cranvr got none). The connect panel listed "This Mac by USB cable" first. A press, and a launch with the Wi-Fi address of the same Mac (`--es host 192.168.50.194:9961 --ez connect true`), both connected over the cable: the host's log shows the peer 127.0.0.1 with TCP media, and the host's `headset` line carries `"ip":"127.0.0.1"`. The Headset card said "Connected by USB cable". The test scene at 60 Hz over the cable: the Tab decoded 59.8 frames a second, the round trip took 1.5 ms, the network 2.0 ms, the decode 5.8 ms, and the host reported 61 ms from motion to display. Wi-Fi to the same Mac gave 24 to 47 frames a second, a round trip of 40 to 100 ms and 180 to 250 ms from motion to display. The Wi-Fi in the room was poor at that time, so these Wi-Fi numbers say little about a good network.
- The Connection choice in the Mac window with a click on Wi-Fi: the app removed the link from the Tab at once, and the running stream stayed on the cable. `adb reconnect` for the Tab ended the cable's TCP link (a removed link alone does not end a running stream). The Tab then connected to the same Mac over Wi-Fi after 3 seconds with UDP media: the host's log shows the peer 192.168.50.97, and the Headset card said "Connected over Wi-Fi". A click on Automatic set the link again within 3 seconds. The running Wi-Fi stream did not move. After the host restarted, the next connection took the cable. The same test ran with a host started by hand on port 9962 and a hand-made `adb reverse`.
- The button "Add USB support" ran with a local zip in the layout of Google's file (`CRANVR_PLATFORM_TOOLS_URL`), on a Mac set to have no adb (`CRANVR_ADB=none`): it unpacked `platform-tools/adb` and checked that it runs. A copy of another version than the running adb server was refused with the notice above.
- Unit tests cover the lines of `adb devices -l` (a device on the cable, unauthorized, offline, an unusual state, wireless debugging, an emulator, the lines of adb's server start), `pm list packages` (the Tab's real answer: the phone build counts, the retired `dev.cranvr.client` and a longer name do not), `adb reverse --list`, the places where adb is looked for, the plan of calls for each choice, a probe of a real host, of a closed link behind `adb reverse` and of nothing, and the headset's choice between the cable and a Wi-Fi entry (the list, the same Mac, another Mac, a dropped cable, a typed address).

## What the tests did not cover

- The cranvr client in a worn headset: the controllers in a game, the carry of the desktop panel by a ray, a recenter and the USB path from the Quest. The cable ran on the Tab only. Nothing here ran with the Quest and with Meta's own adb daemon.
- A pulled cable. `adb reconnect` stood in for it.
- A headset that waits for the user to allow USB debugging, and a Mac that has no adb server running while the downloaded adb starts one. The parser covers the first. The window text was not seen.
- The download from dl.google.com. The test used a local file.
- A headset that has only used the cable and then falls back to Wi-Fi: the fallback stream ran on a Mac that already knew the headset's key (the host with `--trust-all`, and a pairing by file). A new headset meets the card on the Mac there.
- Beat Saber during a song and in a worn headset. The frame rates and latencies in the README come from the game's first screen, the health and safety warning, with the headset on a desk.
- Sound with the Mac muted. A tone at a Mac volume of 15% reached the client at its full level.
- The controller as a mouse on the virtual display in a real headset. The switch of the main display ran with a test client on the Mac only.
- The automatic switch to TCP in the window, for the ALVR client.
- The search and the connect panel on a Quest. The Tab ran them (see above). A router that drops broadcasts leaves the list empty on any device, and the address field is the way around it.
- The click on Allow in the cranvr app. The app's window showed the card for a request from a stand-in host and came to the front. A click of a script did not reach it. The host's side of the answer ran with a line on the host's input.
- A headset of the old protocol against the new Mac, and the reverse, on a device. A hand-written client of version 4 and a hand-written host of version 4 covered both in tests.
