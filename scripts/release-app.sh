#!/bin/zsh
# Makes the cranvr download for other Macs: the app signed with the Developer ID, notarized by
# Apple and stapled, inside a disk image that shows the Applications folder for a drag.
#
# Usage: scripts/release-app.sh [--skip-notarize]
#   --skip-notarize  signs and packs only, for a check of the signed app before a release
#
# Notarization needs a notarytool profile in the keychain, made once by the Apple account owner:
#   xcrun notarytool store-credentials cranvr --apple-id <Apple ID> --team-id <team ID>
# It asks for an app-specific password from account.apple.com. CRANVR_NOTARY_PROFILE names
# another profile.
#
# Output: target/release/cranvr-<version>.dmg

set -euo pipefail

root=${0:A:h:h}
notarize=true
[[ ${1:-} == --skip-notarize ]] && notarize=false
profile=${CRANVR_NOTARY_PROFILE:-cranvr}
cd "${root}"

if ${notarize} && ! xcrun notarytool history --keychain-profile "${profile}" >/dev/null 2>&1; then
    print -u2 "No notarytool profile \"${profile}\" in the keychain. Make it once with:"
    print -u2 "  xcrun notarytool store-credentials ${profile} --apple-id <Apple ID> --team-id <team ID>"
    exit 1
fi

scripts/bundle-app.sh --release
app=${root}/target/release/cranvr.app
version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
work=${root}/target/release/dmg
dmg=${root}/target/release/cranvr-${version}.dmg

# Apple checks the app itself first, so a copy that leaves the disk image opens offline too
if ${notarize}; then
    zip=${root}/target/release/cranvr-notarize.zip
    ditto -c -k --keepParent "${app}" "${zip}"
    xcrun notarytool submit "${zip}" --keychain-profile "${profile}" --wait
    xcrun stapler staple "${app}"
fi

rm -rf "${work}" "${dmg}"
mkdir -p "${work}"
ditto "${app}" "${work}/cranvr.app"
ln -s /Applications "${work}/Applications"
hdiutil create -volname cranvr -srcfolder "${work}" -fs HFS+ -format UDZO -quiet "${dmg}"
identity=$(security find-identity -v -p codesigning | grep -m1 'Developer ID Application' | awk '{print $2}')
codesign --sign "${identity}" --timestamp "${dmg}"

if ${notarize}; then
    xcrun notarytool submit "${dmg}" --keychain-profile "${profile}" --wait
    xcrun stapler staple "${dmg}"
    spctl --assess --type open --context context:primary-signature -vv "${dmg}"
fi
print "Built ${dmg}"
