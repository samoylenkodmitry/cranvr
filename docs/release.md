# Releases

## The Mac app

`scripts/release-app.sh` builds `target/release/cranvr-<version>.dmg`. The app inside is signed with the Developer ID, runs with the hardened runtime and is notarized and stapled, so a Mac that downloads it opens it without a warning. The disk image shows the Applications folder for a drag.

1. Once, the owner of the Apple account makes a notarytool profile. It asks for an app-specific password from account.apple.com:
   `xcrun notarytool store-credentials cranvr --apple-id <Apple ID> --team-id YP97W9MQC8`
2. Raise `version` in the root `Cargo.toml`.
3. Run `scripts/release-app.sh`. Apple's check takes a few minutes.
4. Upload the disk image to the public download page.

`scripts/release-app.sh --skip-notarize` signs and packs without Apple's check. macOS then refuses the app on other Macs. The build on this Mac keeps its "Apple Development" signature from `scripts/bundle-app.sh`, because macOS keeps Screen Recording and Accessibility per signature: a switch of signatures asks for both again.

## The headset app in the Meta Horizon Store

The store needs the owner's Meta developer account. These steps are the owner's.

1. Pick the final application id. Today it is `dev.cranvr.client2` in `client/android/app/build.gradle.kts`. The store keeps the id of the first upload for good.
2. Make an upload key once and keep it safe. A lost key blocks every later update:
   `keytool -genkeypair -v -keystore cranvr-upload.jks -alias cranvr -keyalg RSA -keysize 4096 -validity 10000`
3. Build with the key and a version code above the last upload:
   `CRANVR_KEYSTORE=/path/cranvr-upload.jks CRANVR_KEYSTORE_PASSWORD=... CRANVR_KEY_ALIAS=cranvr scripts/build-client.sh` with `-PcranvrVersionCode=N` in `client/android/gradle.properties` or on the Gradle line.
4. Upload the APK from `target/client/cranvr-client.apk` in the Meta Horizon developer dashboard.

The Mac app and the headset app update apart. Both sides name the side to update when their protocol versions differ.
