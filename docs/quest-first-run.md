# First run on a Quest: Steam Beat Saber at 120 Hz

This list covers a first run with cranvr's Cranpose client on a Quest. The client ran on a Quest 3 on a desk with the host's test scene and with Beat Saber's first screen. Nobody has played a song in a worn headset yet. The stock ALVR client works too: see "With the stock ALVR client" below.

## On the Quest

1. Turn on developer mode for the Quest in the Meta Horizon app on your phone. Connect the Quest to the Mac by USB. Allow USB debugging in the headset.
2. In the cranvr folder, run `scripts/build-client.sh --install`. Add the Quest's serial from `adb devices` when another device is attached.
3. Connect the Quest to the same Wi-Fi as the Mac, on 5 GHz or 6 GHz.

## On the Mac

1. Open `/Applications/cranvr.app`. The host starts by itself.
2. In the Settings tab, under "Headset app", pick "cranvr client". The app then starts the host with `--protocol native`. The Headset card shows this Mac's address with the port, such as `192.168.1.20:9950`.
3. macOS asks for Screen Recording and Accessibility when cranvr starts. Until both are allowed, a card at the top of the window shows an "Allow" button for each. The game sound needs Screen Recording. Accessibility is only for the controller as a mouse in the desktop view.
4. In the Play tab, press "Open Steam" and sign in. Steam may ask for a Steam Guard code.
5. Install Beat Saber in the Steam window. Start the download early.
6. Turn the Mac's volume down. The headset gets the sound at full level anyway.

## Connect

1. In the headset, open the app library and pick "cranvr client" under Unknown Sources. macOS may ask whether cranvr may use the local network: allow it. Without it the host sees no headset.
2. The connect panel shows a code of six digits. Type the Mac's address from the Headset card into the panel with its keypad.
3. On the Mac, type the six digits into the field "Code from the headset" of the Headset card and press "Pair". Pairing stays open for 10 minutes or until one headset pairs.
4. Press "Connect" in the headset. The panel shows the state of the link. A later connection needs no code.
5. The Headset card should read "at 120 Hz". If it reads another rate, pick 120 Hz in the Settings tab. Below it, a line shows the frame rate and the time from motion to display. The stats panel in the headset shows the same numbers.

A Quest that is not on the Mac's network connects over USB. Run `adb reverse tcp:9950 tcp:9950`, then type `127.0.0.1:9950` as the address. This path needs no code.

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

- The panel in the headset says that the host does not know the headset: do the pairing steps again. The pairing window closes after 10 minutes, after one pairing and after five wrong codes.
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

## What the tests did not cover

- The cranvr client in a worn headset: the controllers in a game, the carry of the desktop panel by a ray, a recenter and the USB path from the Quest.
- Beat Saber during a song and in a worn headset. The frame rates and latencies in the README come from the game's first screen, the health and safety warning, with the headset on a desk.
- Sound with the Mac muted. A tone at a Mac volume of 15% reached the client at its full level.
- The controller as a mouse on the virtual display in a real headset. The switch of the main display ran with a test client on the Mac only.
- The automatic switch to TCP in the window, for the ALVR client.
