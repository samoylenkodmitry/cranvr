#!/bin/zsh
# Installs cranvr's game support (Wine with DXMT and the OpenXR runtime) from a packed archive,
# without building anything: the cranvr app's "Set up game support" step runs this.
#
# Usage: scripts/install-game-support.sh [ARCHIVE]
#   Without an argument it downloads the release that game-support-version.txt names from
#   https://github.com/samoylenkodmitry/cranvr-game-support/releases and checks the archive against
#   the SHA-256 in game-support-sha256.txt. Both files sit beside this script, in the repository and
#   in the app, so the app installs only the archive that its own build names. An ARCHIVE from
#   scripts/package-game-support.sh is checked against the .sha256 beside it.
#
# It unpacks the archive and installs the stack with setup-wine.sh into $CRANVR_HOME/wine
# (default: ~/Library/Application Support/cranvr/wine).
# Rosetta 2 must be installed: softwareupdate --install-rosetta --agree-to-license

set -euo pipefail

scripts=${0:A:h}
home=${CRANVR_HOME:-$HOME/Library/Application Support/cranvr}
version=$(tr -d '[:space:]' < "${scripts}/game-support-version.txt")
pinned=$(tr -d '[:space:]' < "${scripts}/game-support-sha256.txt")
release=https://github.com/samoylenkodmitry/cranvr-game-support/releases/download/v${version}/cranvr-game-support-macos.tar.xz

if ! arch -x86_64 /usr/bin/true 2>/dev/null; then
    print -u2 "Rosetta 2 is missing. Install it first: softwareupdate --install-rosetta --agree-to-license"
    exit 1
fi

work=${home}/game-support-download
mkdir -p "${work}"
archive=${work}/cranvr-game-support-macos.tar.xz
if (( $# )); then
    cp "$1" "${archive}"
    if [[ -f $1.sha256 ]]; then
        expected=$(awk '{print $1}' "$1.sha256")
    else
        expected=""
    fi
else
    print "Downloading the game support ${version} (about 320 MB) from ${release%/*}"
    # HTTPS alone, also after a redirect, so nobody on the way can swap the archive's address
    curl --proto '=https' --tlsv1.2 -fL --retry 3 --progress-bar -o "${archive}" "${release}"
    expected=${pinned}
fi

if [[ -n ${expected} ]]; then
    actual=$(shasum -a 256 "${archive}" | awk '{print $1}')
    if [[ ${expected} != "${actual}" ]]; then
        print -u2 "FAIL: the archive's SHA-256 is ${actual}, cranvr expects ${expected}."
        exit 1
    fi
    print "Checked the archive's SHA-256"
fi

print "Unpacking"
unpacked=${work}/unpacked
rm -rf "${unpacked}"
mkdir -p "${unpacked}"
tar -xf "${archive}" -C "${unpacked}"

"${scripts}/setup-wine.sh" "${unpacked}/cranvr-game-support"

rm -rf "${work}"
print "Game support is installed."
