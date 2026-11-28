# Releases

## A release by tag

A tag `vMAJOR.MINOR.PATCH` on a green commit of main makes a release: `git tag v0.2.0 && git push origin v0.2.0`. The tag sets the version. Nobody edits a version by hand. `.github/workflows/release.yml` then builds on the owner's runners (a Mac with the label `idle-mac`, which takes jobs only while it sits idle on power or after the owner agrees, and a Linux machine with the label `samarch1`):

- the Mac app as `cranvr-VERSION.dmg` (`scripts/release-app.sh`);
- the headset app as `cranvr-client-VERSION.apk` (`scripts/build-client.sh`), with the version code `major * 1000000 + minor * 1000 + patch` (`scripts/release/version.sh`).

It publishes both as the GitHub release of the tag. A pull request that changes the release setup runs the same builds on GitHub's runners and publishes nothing: code from a fork never runs on the owner's machines. A pull request from an outside contributor runs no workflow until the owner approves it.

The signatures come from the repository's secrets (Settings, Secrets and variables, Actions). The owner of the Apple and Meta accounts adds them once:

| Secret | What it holds |
|---|---|
| `CRANVR_MACOS_DEVID_P12_BASE64` | The "Developer ID Application" certificate with its private key, exported as .p12, in base64 |
| `CRANVR_MACOS_DEVID_P12_PASSWORD` | The password of that .p12 |
| `CRANVR_ASC_API_KEY_P8_BASE64` | An App Store Connect API key (.p8, in base64) for Apple's notary service |
| `CRANVR_ASC_API_KEY_ID` | The key's id |
| `CRANVR_ASC_API_ISSUER_ID` | The key's issuer id |
| `CRANVR_KEYSTORE_BASE64` | The headset app's upload keystore (see below), in base64 |
| `CRANVR_KEYSTORE_PASSWORD` | Its password, which is also the key's password |
| `CRANVR_KEY_ALIAS` | The key's alias |

Without the Apple secrets the Mac app gets an ad-hoc signature and no notarization. The release notes then tell the user to clear the quarantine flag. Without the keystore secrets the APK carries the runner's debug key, which differs on each run. The release notes then tell the user to uninstall the old app first.

## The Mac app

`scripts/release-app.sh` builds `target/release/cranvr-<version>.dmg`. The app inside is signed with the Developer ID, runs with the hardened runtime and is notarized and stapled, so a Mac that downloads it opens it without a warning. The disk image shows the Applications folder for a drag.

1. Once, the owner of the Apple account makes a notarytool profile. It asks for an app-specific password from account.apple.com:
   `xcrun notarytool store-credentials cranvr --apple-id <Apple ID> --team-id YP97W9MQC8`
2. Run `CRANVR_VERSION=MAJOR.MINOR.PATCH scripts/release-app.sh`. Apple's check takes a few minutes.

A tag release does the same on GitHub and needs no step on a Mac. `scripts/release-app.sh --ad-hoc` builds the disk image with an ad-hoc signature and without Apple's check, as a tag run without the Apple secrets does.

`scripts/release-app.sh --skip-notarize` signs and packs without Apple's check. macOS then refuses the app on other Macs. The build on this Mac keeps its "Apple Development" signature from `scripts/bundle-app.sh`, because macOS keeps Screen Recording and Accessibility per signature: a switch of signatures asks for both again.

## Game support

The Mac app downloads Wine, DXMT, the OpenXR runtime, OpenComposite and the SteamVR driver as one archive from the public repository samoylenkodmitry/cranvr-game-support. It takes the release that `scripts/game-support-version.txt` names and installs the archive only when its SHA-256 equals the one in `scripts/game-support-sha256.txt`. The app holds both files from its build. A change of a runtime, DXMT, OpenComposite or Wine patch or of the SteamVR driver needs a new release, or the apps of other people keep the old runtime.

1. Set a new date in `scripts/game-support-version.txt` (`YYYY.MM.DD`, with `.2` after it for a second release on one day).
2. Run `scripts/build-basaltvr.sh`, `scripts/build-opencomposite.sh`, `scripts/build-wine.sh`, `scripts/build-steamvr-driver.sh` and `scripts/package-game-support.sh`. The archive, its `.sha256`, `cranvr-opencomposite-source.tar.xz` and `cranvr-wine-source.tar.xz` land in `target/game-support`. The archive's SHA-256 also goes into `scripts/game-support-sha256.txt`.
3. Publish the four files, with these names, as a release tagged `v` plus the version: `gh release create v2026.11.19 target/game-support/cranvr-game-support-macos.tar.xz target/game-support/cranvr-game-support-macos.tar.xz.sha256 target/game-support/cranvr-opencomposite-source.tar.xz target/game-support/cranvr-wine-source.tar.xz --repo samoylenkodmitry/cranvr-game-support --title "Game support 2026-11-19" --notes-file NOTES.md`. The notes say in plain words what changed. OpenComposite is GPL 3: its source must stay with each release that carries its DLL. Wine is LGPL 2.1: the source of the patched Wine files stays with each release too. The archive holds the same source in `SOURCE/`, with the licenses in `LICENSES/`, and the repository's README says so.
4. Commit `scripts/game-support-version.txt` and `scripts/game-support-sha256.txt` together, and build the Mac app from that commit. Its Play tab then offers "Update game support" to every Mac whose stamp in `wine/game-support-version.txt` is older. A rebuilt archive has another SHA-256: publish the archive whose SHA-256 the file holds.

Do step 3 before the app with the new version file reaches anyone. An app that names a version that no release carries fails its download of the game support.

## The headset app in the Meta Horizon Store

The store needs the owner's Meta developer account. These steps are the owner's.

1. Pick the final application id. Today it is `dev.cranvr.client` in `client/android/app/build.gradle.kts`. The store keeps the id of the first upload for good.
2. Make an upload key once and keep it safe. A lost key blocks every later update:
   `keytool -genkeypair -v -keystore cranvr-upload.jks -alias cranvr -keyalg RSA -keysize 4096 -validity 10000`
3. Build with the key and a version code above the last upload:
   `CRANVR_KEYSTORE=/path/cranvr-upload.jks CRANVR_KEYSTORE_PASSWORD=... CRANVR_KEY_ALIAS=cranvr scripts/build-client.sh` with `-PcranvrVersionCode=N` in `client/android/gradle.properties` or on the Gradle line.
4. Upload the APK from `target/client/cranvr-client.apk` in the Meta Horizon developer dashboard.

The Mac app and the headset app of one release come from one commit. cranvr supports one protocol version. Both sides name the side to update when their protocol versions differ.
