#!/usr/bin/env bash
# Prints the release version as lines for $GITHUB_ENV:
#   CRANVR_VERSION       the tag without its "v" (a tag run) or Cargo.toml's version (any other run)
#   CRANVR_VERSION_CODE  the Android version code: major * 1000000 + minor * 1000 + patch
# A release tag must be vMAJOR.MINOR.PATCH, because the store needs a version code that grows.
set -euo pipefail
if [[ ${GITHUB_REF_TYPE:-} == tag ]]; then
  version=${GITHUB_REF_NAME#v}
else
  version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
fi
if [[ ! ${version} =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "The version \"${version}\" is not MAJOR.MINOR.PATCH" >&2
  exit 1
fi
major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}
if (( minor > 999 || patch > 999 )); then
  echo "The version \"${version}\" has a part above 999" >&2
  exit 1
fi
echo "CRANVR_VERSION=${version}"
echo "CRANVR_VERSION_CODE=$(( 10#${major} * 1000000 + 10#${minor} * 1000 + 10#${patch} ))"
