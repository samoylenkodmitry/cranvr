#!/bin/zsh
# Makes the cranvr download for other Macs: the app signed with the Developer ID, notarized by
# Apple and stapled, inside a disk image that shows the Applications folder for a drag.
#
# Usage: scripts/release-app.sh [--skip-notarize | --ad-hoc]
#   --skip-notarize  signs and packs only, for a check of the signed app before a release
#   --ad-hoc         signs with an ad-hoc signature and skips Apple's check: the build of a CI run
#                    that has no signing secrets. macOS then refuses the app on other Macs.
#
# Notarization takes the App Store Connect API key when CRANVR_ASC_KEY (the .p8 file),
# CRANVR_ASC_KEY_ID and CRANVR_ASC_ISSUER_ID are set, as in CI. Otherwise it takes a notarytool
# profile in the keychain, made once by the Apple account owner:
#   xcrun notarytool store-credentials cranvr --apple-id <Apple ID> --team-id <team ID>
# It asks for an app-specific password from account.apple.com. CRANVR_NOTARY_PROFILE names
# another profile.
#
# CRANVR_VERSION sets the version (the release tag without its "v"). It defaults to Cargo.toml's.
#
# Output: target/release/cranvr-<version>.dmg
set -euo pipefail
root=${0:A:h:h}
mode=notarize
case ${1:-} in
    --skip-notarize) mode=sign ;;
    --ad-hoc) mode=ad-hoc ;;
    '') ;;
    *) print -u2 "Unknown argument: $1"; exit 1 ;;
esac
profile=${CRANVR_NOTARY_PROFILE:-cranvr}
cd "${root}"
if [[ -n ${CRANVR_ASC_KEY:-} ]]; then
    notary_auth=(--key "${CRANVR_ASC_KEY}" --key-id "${CRANVR_ASC_KEY_ID:?}" --issuer "${CRANVR_ASC_ISSUER_ID:?}")
else
    notary_auth=(--keychain-profile "${profile}")
fi
if [[ ${mode} == notarize ]] && ! xcrun notarytool history "${notary_auth[@]}" >/dev/null 2>&1; then
    print -u2 "Apple's notary service refuses the credentials. Make a keychain profile once with:"
    print -u2 "  xcrun notarytool store-credentials ${profile} --apple-id <Apple ID> --team-id <team ID>"
    exit 1
fi
if [[ ${mode} == ad-hoc ]]; then
    CRANVR_SIGN_IDENTITY=- scripts/bundle-app.sh
else
    scripts/bundle-app.sh --release
fi
app=${root}/target/release/cranvr.app
version=${CRANVR_VERSION:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}
work=${root}/target/release/dmg
dmg=${root}/target/release/cranvr-${version}.dmg
# Apple checks the app itself first, so a copy that leaves the disk image opens offline too
if [[ ${mode} == notarize ]]; then
    zip=${root}/target/release/cranvr-notarize.zip
    ditto -c -k --keepParent "${app}" "${zip}"
    xcrun notarytool submit "${zip}" "${notary_auth[@]}" --wait
    xcrun stapler staple "${app}"
fi
rm -rf "${work}" "${dmg}"
mkdir -p "${work}"
ditto "${app}" "${work}/cranvr.app"
ln -s /Applications "${work}/Applications"
hdiutil create -volname cranvr -srcfolder "${work}" -fs HFS+ -format UDZO -quiet "${dmg}"
if [[ ${mode} != ad-hoc ]]; then
    identity=${CRANVR_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | grep -m1 'Developer ID Application' | awk '{print $2}')}
    codesign --sign "${identity}" --timestamp "${dmg}"
fi
if [[ ${mode} == notarize ]]; then
    xcrun notarytool submit "${dmg}" "${notary_auth[@]}" --wait
    xcrun stapler staple "${dmg}"
    spctl --assess --type open --context context:primary-signature -vv "${dmg}"
fi
print "Built ${dmg}"
