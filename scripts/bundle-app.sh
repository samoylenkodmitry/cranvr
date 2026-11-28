#!/bin/zsh
# Builds cranvr.app: the window (cranvr), the host (cranvr-host), the setup scripts and the program
# that starts and stops the headset's own recorder (scripts/build-quest-recorder.sh).
#
# Usage: scripts/bundle-app.sh [--install] [--release]
#   --install  copies the app to /Applications
#   --release  signs for people who download the app: the "Developer ID Application" identity
#              with the hardened runtime and a secure timestamp, as notarization needs.
#              scripts/release-app.sh runs this, then notarizes the app.
#
# macOS keeps permissions (Screen Recording, System Audio Recording, Accessibility, Local Network, Microphone) per signed app. The
# script signs with your first "Apple Development" identity, so a rebuilt app keeps them.
# Set CRANVR_SIGN_IDENTITY to choose another identity, or "-" for an ad-hoc signature (then
# macOS asks again after each build).
# CRANVR_VERSION sets the version. It defaults to Cargo.toml's.

set -euo pipefail

root=${0:A:h:h}
install=false
release=false
for arg in "$@"; do
    case ${arg} in
        --install) install=true ;;
        --release) release=true ;;
        *) print -u2 "Unknown argument: ${arg}"; exit 1 ;;
    esac
done

cd "${root}"
cargo build --release -p cranvr-app -p cranvr-host
scripts/build-quest-recorder.sh

app=${root}/target/release/cranvr.app
rm -rf "${app}"
mkdir -p "${app}/Contents/MacOS" "${app}/Contents/Resources/scripts"
cp target/release/cranvr target/release/cranvr-host "${app}/Contents/MacOS/"
cp scripts/setup-wine.sh scripts/setup-steam.sh scripts/install-game-support.sh scripts/run-game.sh scripts/game-support-version.txt scripts/game-support-sha256.txt "${app}/Contents/Resources/scripts/"
cp target/quest-recorder/cranvr-quest-recorder.jar "${app}/Contents/Resources/"
version=${CRANVR_VERSION:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}

cat > "${app}/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>dev.cranvr.cranvr</string>
    <key>CFBundleName</key><string>cranvr</string>
    <key>CFBundleDisplayName</key><string>cranvr</string>
    <key>CFBundleExecutable</key><string>cranvr</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${version}</string>
    <key>CFBundleVersion</key><string>${version}</string>
    <key>LSMinimumSystemVersion</key><string>14.2</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.games</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>cranvr finds your headset on the local network and streams to it.</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>Games on this Mac hear the input device that you chose in cranvr's settings.</string>
    <key>NSAudioCaptureUsageDescription</key>
    <string>cranvr plays your Mac's sound in the headset.</string>
</dict>
</plist>
EOF

kind='Apple Development'
sign_options=()
if ${release}; then
    kind='Developer ID Application'
    # The hardened runtime lets a game under Wine, which cranvr starts, record from a Mac input device
    # (the Settings tab's "Microphone in games") only with this entitlement
    entitlements=${root}/target/release/cranvr.entitlements
    cat > "${entitlements}" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.device.audio-input</key><true/>
</dict>
</plist>
EOF
    sign_options=(--options runtime --timestamp --entitlements "${entitlements}")
fi
identity=${CRANVR_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | grep -m1 "${kind}" | awk '{print $2}' || true)}
if ${release} && [[ -z ${identity} || ${identity} == - ]]; then
    print -u2 "A release needs a \"Developer ID Application\" identity in the keychain."
    exit 1
fi
identity=${identity:--}
codesign --force "${sign_options[@]}" --sign "${identity}" --identifier dev.cranvr.host "${app}/Contents/MacOS/cranvr-host"
codesign --force "${sign_options[@]}" --sign "${identity}" --identifier dev.cranvr.cranvr "${app}"
codesign --verify --deep --strict "${app}"
print "Built ${app}"

if ${install}; then
    rm -rf /Applications/cranvr.app
    ditto "${app}" /Applications/cranvr.app
    print "Installed /Applications/cranvr.app"
fi
