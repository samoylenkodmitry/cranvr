# First run on a Quest: Steam Beat Saber at 120 Hz

This list covers a first run with cranvr's Cranpose client on a Quest.

## On the Quest

1. Turn on developer mode for the Quest in the Meta Horizon app on your phone. The install of a test build needs it. Connect the Quest to the Mac by USB. Allow USB debugging in the headset. The cable is optional for the stream, and the stream over the cable needs no developer mode: see "The USB cable" below.
2. In the cranvr folder, run `scripts/build-client.sh --install`. Add the Quest's serial from `adb devices` when another device is attached.
3. Connect the Quest to the same Wi-Fi as the Mac, on 5 GHz or 6 GHz.

## On the Mac

1. Open `/Applications/cranvr.app`. The host starts by itself.
2. The Headset card says "Open cranvr on the headset. It finds this Mac." Below it, a line shows this Mac's address with the port, such as `192.168.1.20:9950`. You need the address only when the headset finds nothing.
3. macOS asks for Screen Recording, System Audio Recording and Accessibility when cranvr starts. Until all three are allowed, a card at the top of the window shows an "Allow" button for each. The game sound needs System Audio Recording. Accessibility is only for the controller as a mouse in the desktop view.
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

A headset that sits on a USB cable at the Mac connects over the cable without a typed address. It needs no Wi-Fi. The headset's USB network needs no Developer Mode:

1. Plug the headset into the Mac and open the cranvr app in the headset. The headset switches its USB functions to a network over the cable.
2. The first time, macOS asks "Allow accessory to connect?". Click Allow. macOS remembers the answer for the headset.
3. The connect panel in the headset lists "This Mac by USB cable" first. Press it. A headset that the Mac does not know shows a code once, as over Wi-Fi. Allow it on the Mac. The Mac window then says "Connected by USB cable".

With Developer Mode on, `adb reverse` runs over the same cable too. The headset prefers its USB network when both answer. This is how `adb reverse` goes:

1. Developer mode must be on for the headset (see "On the Quest"). Plug the headset into the Mac. The headset asks to allow USB debugging: allow it. Until then the Headset card in the Mac window says "Put on the headset and allow USB debugging."
2. The Mac looks for the adb tool of Android (the PATH, Android Studio's SDK, Homebrew's android-platform-tools). A Mac with none shows the button "Add USB support" in the Settings tab, under Connection. The button downloads Google's Android platform-tools for macOS (about 16 MB) from dl.google.com into cranvr's folder. A Mac that has a headset on the cable but no cranvr app on it does nothing: the Mac counts a device only when `dev.cranvr.client` is installed on it.
3. The Mac then sets up the link by itself (`adb reverse` for the host's port). The Headset card says "The headset is on the USB cable. It connects over the cable."
4. The connect panel in the headset lists "This Mac by USB cable" first. Press it. The cable needs no code. The Wi-Fi line of the same Mac does not show: each Mac has one line. The cable wins. A typed address of the same Mac takes the cable too. The Mac window then says "Connected by USB cable". Over Wi-Fi it says "Connected over Wi-Fi".

The Settings tab has a Connection choice. "Automatic" uses the cable while the headset is plugged in and Wi-Fi otherwise. "Wi-Fi" takes the link over the cable away on both ways, the USB network and `adb reverse`: the Mac tells the headset at each connection, so the headset connects to it over Wi-Fi with the cable plugged in. "USB cable" keeps the link. The Headset card says when the headset is not on the cable ("Plug the headset into this Mac with a USB cable.") or when a headset connects over Wi-Fi anyway. A change counts from the next connection: a running stream keeps its path.

When the cable comes out, the headset connects to the same Mac over Wi-Fi by itself, after about three seconds. It finds the Mac by its key's fingerprint. A headset that has only used the cable has not paired with the Mac. Its first Wi-Fi connection shows a code. The Mac asks you to allow the headset, as in "Connect" above. A Wi-Fi link never moves to the cable while it runs. When it ends, the next connection uses the cable if the headset is plugged in.

An adb tool stops a running adb server of another version. The downloaded copy of adb could therefore stop the server of Android Studio. The Mac uses the downloaded copy only when no adb server of another version runs. The window says "Another adb tool runs on this Mac. Close it, then cranvr can use the cable." otherwise.

The address entry stays for a Mac on another network, which the search does not reach. Without the Mac's setup, `adb reverse tcp:9950 tcp:9950` and the address `127.0.0.1:9950` still work. The client keeps the last address that you pressed or typed. A start with the extras `--es host ADDRESS --ez connect true` connects to a Mac without a press (`adb shell am start ...`). The extra `--ei usb_port PORT` moves the port that the headset asks on the cable, for a test host on another port.

The Mac and the headset speak one protocol version, and both apps come from one commit. When the versions differ, there is no session. The connect panel shows "Update cranvr on the Mac." or "Update the cranvr app on the headset." on the Mac's entry, and the Mac shows a note that names the side to update.

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

- The headset does not list "This Mac by USB cable": the headset's USB network needs the cranvr app in front in the headset and the Allow of macOS's question "Allow accessory to connect?". The headset's log says "the USB network is up on usb0" when the network works. Without it the cable line comes from `adb reverse`: the Mac window must say "The headset is on the USB cable." (Headset card, no headset connected). It does not when the Connection choice in the Settings tab is Wi-Fi, when the headset has no developer mode, when the cranvr app is not installed on the headset, when this Mac has no adb tool (press "Add USB support") or when the headset waits for you to allow USB debugging. The Log tab lines that start with "USB:" show what the Mac did. A cable that the headset cannot reach shows no line at all, because the headset asks the Mac itself and a closed link gives no answer.

- The panel in the headset shows a code and "Allow it on the Mac": look at the cranvr window on the Mac and press Allow. The headset says "Nobody answered on the Mac" after three minutes. Press the Mac again to ask once more. It says "The Mac denied this headset" after a Deny.
- The list in the headset stays empty: check that the Mac and the headset use the same Wi-Fi. A router with client isolation blocks the search. Check that cranvr may use the local network in System Settings, under Privacy & Security. Check that the host runs: the cranvr window says "Host running". Use "Enter an address" as the way around.
- The panel in the headset says that the host does not have the key that the headset paired with. This happens after a new install of the host. Press "Forget host key" in the connect panel, then connect again.
- The headset does not connect: check the Wi-Fi. Check that cranvr may use the local network in System Settings, under Privacy & Security. Then read the Log tab. A notice in orange names network problems.
- No sound in the headset: the headset card says why. Check System Audio Recording in the Desktop tab. The host starts the capture within a second of the permission. If it reads "not allowed" while System Settings shows cranvr as allowed under "Screen & System Audio Recording", remove cranvr from that list, allow it again, then quit and reopen cranvr.
- Frames drop at 120 Hz, or the Log tab says that the game is below the headset's rate: keep "Game anti-aliasing" at "At most 2x" in the Settings tab. On the base M5 Beat Saber's first screen ran at 91 frames a second with 4x and at 119 with 2x. A game that stays slow for a minute renders smaller at its next start by itself. A smaller Game render size in the Settings tab lowers every game. Keep "Panel pixels while a game runs" on: a TV in a scaled mode costs the Mac's desktop far more GPU time. Turning the Desktop view off also saves GPU time.
- Beat Saber starts but stays black. Its process uses no CPU. Wine's desktop process may hang. This happened once after a day of display tests. Quit Steam and the game. Run `WINEPREFIX="$HOME/Library/Application Support/cranvr/wine/prefix" "$HOME/Library/Application Support/cranvr/wine/engine/bin/wineserver" -k`. Then start Steam again. The cause is not known.
- Beat Saber does not start: `~/Library/Application Support/cranvr/game.log` and `game.err.log` hold its output.
- Send me the line under the headset card and the end of the Log tab.

## Not tested yet

- The client in a worn headset: the controllers in a game, the carry of the desktop panel by a ray, a recenter and the USB cable from a Quest.
- Beat Saber during a song and in a worn headset. The frame rates and latencies come from the game's first screen, the health and safety warning, with the headset on a desk.
- Sound with the Mac muted.
- The controller as a mouse on the virtual display in a real headset.
- The search and the connect panel on a Quest with a router that drops broadcasts. The address field is the way around it.
- A pulled USB cable. `adb reconnect` stands in for it in tests.
- A headset that waits for you to allow USB debugging.
- The first start of an adb server by the downloaded adb.
- The click on Allow in the cranvr window by a user. The host's side of the answer is tested with a line on the host's input.
