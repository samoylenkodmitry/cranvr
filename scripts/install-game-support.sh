#!/bin/zsh
# Installs cranvr's game support (Wine with DXMT and the OpenXR runtime) from a packed archive,
# without building anything: the cranvr app's "Set up game support" step runs this.
#
# Usage: scripts/install-game-support.sh [ARCHIVE|URL]
#   Without an argument it downloads the newest archive from the public release:
#   https://github.com/samoylenkodmitry/cranvr-game-support/releases/latest
#
# It checks the archive against its .sha256, unpacks it, and installs the stack with
# setup-wine.sh into $CRANVR_HOME/wine (default: ~/Library/Application Support/cranvr/wine).
# Rosetta 2 must be installed: softwareupdate --install-rosetta --agree-to-license

set -euo pipefail

readonly default_url=https://github.com/samoylenkodmitry/cranvr-game-support/releases/latest/download/cranvr-game-support-macos.tar.xz
scripts=${0:A:h}
home=${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}
source=${1:-${default_url}}

if ! arch -x86_64 /usr/bin/true 2>/dev/null; then
    print -u2 "Rosetta 2 is missing. Install it first: softwareupdate --install-rosetta --agree-to-license"
    exit 1
fi

work=${home}/game-support-download
mkdir -p "${work}"
archive=${work}/cranvr-game-support-macos.tar.xz
if [[ ${source} == https://* ]]; then
    print "Downloading the game support (about 250 MB) from ${source%/*}"
    curl -fL --retry 3 --progress-bar -o "${archive}" "${source}"
    curl -fsSL --retry 3 -o "${archive}.sha256" "${source}.sha256"
else
    cp "${source}" "${archive}"
    if [[ -f ${source}.sha256 ]]; then
        cp "${source}.sha256" "${archive}.sha256"
    fi
fi

if [[ -f ${archive}.sha256 ]]; then
    expected=$(awk '{print $1}' "${archive}.sha256")
    actual=$(shasum -a 256 "${archive}" | awk '{print $1}')
    if [[ ${expected} != "${actual}" ]]; then
        print -u2 "FAIL: the archive's SHA-256 is ${actual}, the release says ${expected}."
        exit 1
    fi
    print "Checked the archive's SHA-256"
elif [[ ${source} == https://* ]]; then
    print -u2 "FAIL: no .sha256 next to ${source}"
    exit 1
fi

print "Unpacking"
unpacked=${work}/unpacked
rm -rf "${unpacked}"
mkdir -p "${unpacked}"
tar -xf "${archive}" -C "${unpacked}"

"${scripts}/setup-wine.sh" "${unpacked}/cranvr-game-support"

rm -rf "${work}"
print "Game support is installed. Next: set up Steam in the cranvr app."
