# First run on a Quest: Steam Beat Saber at 120 Hz

cranvr has not run with a real Quest yet. This list covers what the tests on this Mac and on the Galaxy Tab S7+ could not.

## On the Quest

1. Install ALVR from the Meta store. The store has 20.14.0. cranvr works with every 20.x version. A v21 nightly build does not connect. The cranvr window then shows a notice.
2. Connect the Quest to the same Wi-Fi as the Mac, on 5 GHz or 6 GHz.
3. If Tailscale runs on the Quest, exclude the ALVR app from it in the Tailscale app. Without that, cranvr's "Network: Auto" setting moves the stream to TCP after about 10 seconds. TCP adds about 2 ms.

### cranvr's own Quest app (optional)

The store app is the route with tests behind it. Try it first.

`target/quest-client/cranvr-quest.apk` is cranvr's own Quest app. It is ALVR's v20.14.1 client under its own package, `dev.cranvr.client`, labeled "cranvr". It gives the same picture as the store app today. Native mode will build on it. It has not run on a Quest.

1. Turn on developer mode for the Quest in the Meta Horizon app on your phone. Connect the Quest to the Mac by USB. Allow USB debugging in the headset.
2. In the cranvr folder, run `scripts/build-quest.sh --install`.
3. In the headset, open the app library and pick "cranvr" under Unknown Sources.
4. It shows up in the cranvr window as its own headset with its own "Pair" button.

## On the Mac

1. Open `/Applications/cranvr.app`. The host starts by itself.
2. macOS asks for Screen Recording and Accessibility when cranvr starts. Until both are allowed, a card at the top of the window shows an "Allow" button for each. The game sound needs Screen Recording. Accessibility is only for the controller as a mouse in the desktop view.
3. In the Play tab, press "Open Steam" and sign in. Steam may ask for a Steam Guard code.
4. Install Beat Saber in the Steam window. Start the download early.
5. Turn the Mac's volume down. The headset gets the sound at full level anyway.

## Connect

1. Open ALVR on the Quest. macOS may ask whether cranvr may use the local network: allow it. Without it no headset shows up. The headset shows up in the cranvr window with a "Pair with ..." button. Press it.
2. On the first connection the Quest shows "The streamer is restarting" once and reconnects by itself.
3. The headset card should read "at 120 Hz". If it reads another rate, pick 120 Hz in the Settings tab. Below it, a line shows the frame rate and the time from motion to display.

## Work from the headset

The headset shows the Mac's main display as it is. With cranvr's Quest app it shows the display as a sharp layer, with video at up to 60 fps and the cursor drawn by the headset.

- To add a display that only the headset shows, pick "Add a display" in the Desktop tab. It sits to the right of the TV. No window moves by itself: drag windows onto it with the Mac's mouse.
- Point at a window with a controller to move the mouse. Pull the trigger to click. Push the thumbstick to scroll. Type on the Mac's keyboard.
- To move the screen, point at the bar under it, hold the trigger and move the controller. Push the thumbstick to move the screen away or near. Click the right thumbstick to bring the screen in front of you.
- To resize the screen, point just outside a corner, hold the trigger and pull.
- The buttons beside the bar pause the picture, set its quality and add a screen with a new Mac display. "Close" removes an added screen.
- With an added display, "Main display while the headset is on" moves everything into VR while the headset streams. The menu bar, the Dock and your windows then move from the TV into VR. They go back when the stream ends. "Bring the cranvr window into VR" moves only the cranvr window. Both are off by default.

## Play

1. Press "Play" next to Beat Saber in the cranvr window on the Mac, then put on the headset. To press it from the headset, first drag the cranvr window onto the virtual display.
2. In Beat Saber, open Settings, then Audio. Run the audio latency calibration once. ALVR buffers about 50 ms of sound.

## When something goes wrong

- The headset connects, but no picture comes after about 20 seconds: pick TCP under Network in the Settings tab. The automatic switch to TCP has not run in the window yet.
- No headset in the window: check the Wi-Fi. Check that cranvr may use the local network in System Settings, under Privacy & Security. Then read the Log tab. A notice in orange names network problems.
- No sound in the headset: the headset card says why. Check Screen Recording in the Desktop tab. If it reads "not allowed" while System Settings shows cranvr as allowed, remove cranvr from that list, allow it again, then quit and reopen cranvr. The host tries the capture once per start: turn "Game sound in the headset" off and on in Settings to try again.
- Frames drop at 120 Hz, or the Log tab says that the game is below the headset's rate: keep "Game anti-aliasing" at "At most 2x" in the Settings tab. On the base M5 Beat Saber's first screen ran at 91 frames a second with 4x and at 119 with 2x. A game that stays slow for a minute renders smaller at its next start by itself. A smaller Game render size in the Settings tab lowers every game. Keep "Panel pixels while a game runs" on: a TV in a scaled mode costs the Mac's desktop far more GPU time. Turning the Desktop view off also saves GPU time.
- The game feels late: try the Low latency switch in the Settings tab. It saved about 4 ms in a test on a desk. If the picture then stutters, turn it off.
- Beat Saber starts but stays black. Its process uses no CPU. Wine's desktop process may hang. This happened once after a day of display tests. Quit Steam and the game. Run `WINEPREFIX="$HOME/Library/Application Support/cranvr/wine/prefix" "$HOME/Library/Application Support/cranvr/wine/engine/bin/wineserver" -k`. Then start Steam again. The cause is not known.
- Beat Saber does not start: `~/Library/Application Support/cranvr/game.log` and `game.err.log` hold its output.
- Send me the line under the headset card and the end of the Log tab.

## What the tests covered

- The stream at 120 Hz to the Galaxy Tab S7+ through ALVR's own client core and the Quest app's decoder path: 119.5 to 120 frames a second, 34 ms from motion to display, 97% of the frames shown.
- A real Direct3D 11 OpenXR program under Wine at 120 Hz to the Tab: 119.7 frames a second.
- The game sound from the Mac to the client: 20.5 seconds of sound in a 20-second run, over USB with TCP and over Wi-Fi with UDP.
- The notice for an ALVR app of another version: a discovery packet with another protocol ID produced it.

## What the tests did not cover

- A Quest and its compositor.
- Beat Saber during a song and in a worn headset. The frame rates and latencies in the README come from the game's first screen, the health and safety warning, with the headset on a desk.
- Sound with the Mac muted. A tone at a Mac volume of 15% reached the client at its full level.
- The controller as a mouse on the virtual display in a real headset. The switch of the main display ran with a test client on the Mac only.
- The automatic switch to TCP in the window.
- cranvr's own Quest app on a Quest. On the Galaxy Tab S7+ it starts and loads its code with the OpenXR loader. The Tab has no OpenXR runtime.
