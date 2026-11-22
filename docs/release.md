# Releases

## The Mac app

`scripts/release-app.sh` builds `target/release/cranvr-<version>.dmg`. The app inside is signed with the Developer ID, runs with the hardened runtime and is notarized and stapled, so a Mac that downloads it opens it without a warning. The disk image shows the Applications folder for a drag.

1. Once, the owner of the Apple account makes a notarytool profile. It asks for an app-specific password from account.apple.com:
   `xcrun notarytool store-credentials cranvr --apple-id <Apple ID> --team-id YP97W9MQC8`
2. Raise `version` in the root `Cargo.toml`.
3. Run `scripts/release-app.sh`. Apple's check takes a few minutes.
4. Upload the disk image to the public download page.

`scripts/release-app.sh --skip-notarize` signs and packs without Apple's check. macOS then refuses the app on other Macs. The build on this Mac keeps its "Apple Development" signature from `scripts/bundle-app.sh`, because macOS keeps Screen Recording and Accessibility per signature: a switch of signatures asks for both again.

## Game support

The Mac app downloads Wine, DXMT, the OpenXR runtime, OpenComposite and the SteamVR driver as one archive from the public repository samoylenkodmitry/cranvr-game-support, in its newest release. A change of a runtime, DXMT, OpenComposite or Wine patch or of the SteamVR driver needs a new release, or the apps of other people keep the old runtime.

1. Set a new date in `scripts/game-support-version.txt` (`YYYY.MM.DD`, with `.2` after it for a second release on one day).
2. Run `scripts/build-basaltvr.sh`, `scripts/build-opencomposite.sh`, `scripts/build-wine.sh`, `scripts/build-steamvr-driver.sh` and `scripts/package-game-support.sh`. The archive, its `.sha256`, `cranvr-opencomposite-source.tar.xz` and `cranvr-wine-source.tar.xz` land in `target/game-support`.
3. Publish the four files, with these names, as a release tagged `v` plus the version: `gh release create v2026.11.19 target/game-support/cranvr-game-support-macos.tar.xz target/game-support/cranvr-game-support-macos.tar.xz.sha256 target/game-support/cranvr-opencomposite-source.tar.xz target/game-support/cranvr-wine-source.tar.xz --repo samoylenkodmitry/cranvr-game-support --title "Game support 2026-11-19" --notes-file NOTES.md`. The notes say in plain words what changed. OpenComposite is GPL 3: its source must stay with each release that carries its DLL. Wine is LGPL 2.1: the source of the patched Wine files stays with each release too. The archive holds the same source in `SOURCE/`, with the licenses in `LICENSES/`, and the repository's README says so.
4. Build the Mac app with the same version file. Its Play tab then offers "Update game support" to every Mac whose stamp in `wine/game-support-version.txt` is older.

Do step 3 before the app with the new version file reaches anyone. An app that expects a version that no release carries offers the update again after each install.

## The headset app in the Meta Horizon Store

The store needs the owner's Meta developer account. These steps are the owner's.

1. Pick the final application id. Today it is `dev.cranvr.client` in `client/android/app/build.gradle.kts`. The store keeps the id of the first upload for good.
2. Make an upload key once and keep it safe. A lost key blocks every later update:
   `keytool -genkeypair -v -keystore cranvr-upload.jks -alias cranvr -keyalg RSA -keysize 4096 -validity 10000`
3. Build with the key and a version code above the last upload:
   `CRANVR_KEYSTORE=/path/cranvr-upload.jks CRANVR_KEYSTORE_PASSWORD=... CRANVR_KEY_ALIAS=cranvr scripts/build-client.sh` with `-PcranvrVersionCode=N` in `client/android/gradle.properties` or on the Gradle line.
4. Upload the APK from `target/client/cranvr-client.apk` in the Meta Horizon developer dashboard.

The Mac app and the headset app of one release come from one commit. cranvr supports one protocol version. Both sides name the side to update when their protocol versions differ.
