#!/bin/zsh
# Builds cranvr.app: the window (cranvr), the host (cranvr-host) and the setup scripts.
#
# Usage: scripts/bundle-app.sh [--install] [--release]
#   --install  copies the app to /Applications
#   --release  signs for people who download the app: the "Developer ID Application" identity
#              with the hardened runtime and a secure timestamp, as notarization needs.
#              scripts/release-app.sh runs this, then notarizes the app.
#
# macOS keeps permissions (Screen Recording, Accessibility, Local Network) per signed app. The
# script signs with your first "Apple Development" identity, so a rebuilt app keeps them.
# Set CRANVR_SIGN_IDENTITY to choose another identity, or "-" for an ad-hoc signature (then
# macOS asks again after each build).

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

app=${root}/target/release/cranvr.app
rm -rf "${app}"
mkdir -p "${app}/Contents/MacOS" "${app}/Contents/Resources/scripts"
cp target/release/cranvr target/release/cranvr-host "${app}/Contents/MacOS/"
cp scripts/setup-wine.sh scripts/setup-steam.sh scripts/install-game-support.sh scripts/run-game.sh scripts/game-support-version.txt "${app}/Contents/Resources/scripts/"
version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)

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
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.games</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>cranvr finds your headset on the local network and streams to it.</string>
</dict>
</plist>
EOF

kind='Apple Development'
sign_options=()
if ${release}; then
    kind='Developer ID Application'
    sign_options=(--options runtime --timestamp)
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
