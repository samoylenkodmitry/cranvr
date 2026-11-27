#!/usr/bin/env bash
# Puts the Developer ID identity from the CI secrets into a keychain of its own for one run
# (prepare) and removes it after the run (cleanup). scripts/bundle-app.sh signs with the identity
# that prepare writes to CRANVR_SIGN_IDENTITY.
set -euo pipefail

prepare() {
  : "${P12_BASE64:?Missing CRANVR_MACOS_DEVID_P12_BASE64}"
  : "${P12_PASSWORD:?Missing CRANVR_MACOS_DEVID_P12_PASSWORD}"
  : "${RUNNER_TEMP:?Missing RUNNER_TEMP}"
  : "${GITHUB_ENV:?Missing GITHUB_ENV}"
  umask 077
  CRANVR_SIGN_STATE=$(mktemp -d "$RUNNER_TEMP/cranvr-developer-id.XXXXXX")
  CRANVR_SIGN_KEYCHAIN="$CRANVR_SIGN_STATE/signing.keychain-db"
  CRANVR_SIGN_KEYCHAIN_PASS=$(openssl rand -base64 32)
  export CRANVR_SIGN_KEYCHAIN_PASS
  echo "::add-mask::$CRANVR_SIGN_KEYCHAIN_PASS"
  printf 'CRANVR_SIGN_STATE=%s\nCRANVR_SIGN_KEYCHAIN=%s\nCRANVR_SIGN_KEYCHAIN_PASS=%s\n' \
    "$CRANVR_SIGN_STATE" "$CRANVR_SIGN_KEYCHAIN" "$CRANVR_SIGN_KEYCHAIN_PASS" >> "$GITHUB_ENV"
  trap 'rm -f "$CRANVR_SIGN_STATE"/*.p12 "$CRANVR_SIGN_STATE"/*.pem' EXIT
  printf '%s' "$P12_BASE64" | base64 -d > "$CRANVR_SIGN_STATE/input.p12"

  # Import only the identity from the secret. Bundled CA certificates must not
  # shadow the runner's system trust anchors or Apple's current intermediate.
  # Older Keychain exports use RC2, which OpenSSL 3 reads via its legacy provider.
  local read_options=(-passin env:P12_PASSWORD)
  local pkcs12_help
  pkcs12_help=$(openssl pkcs12 -help 2>&1 || true)
  if [[ "$pkcs12_help" == *"-legacy"* ]]; then
    read_options+=(-legacy)
  fi
  openssl pkcs12 -in "$CRANVR_SIGN_STATE/input.p12" "${read_options[@]}" \
    -clcerts -nokeys -out "$CRANVR_SIGN_STATE/leaf.pem"
  test "$(grep -c 'BEGIN CERTIFICATE' "$CRANVR_SIGN_STATE/leaf.pem")" = 1
  openssl pkcs12 -in "$CRANVR_SIGN_STATE/input.p12" "${read_options[@]}" \
    -nocerts -out "$CRANVR_SIGN_STATE/key.pem" -passout env:CRANVR_SIGN_KEYCHAIN_PASS
  openssl pkcs12 -export -in "$CRANVR_SIGN_STATE/leaf.pem" \
    -inkey "$CRANVR_SIGN_STATE/key.pem" -passin env:CRANVR_SIGN_KEYCHAIN_PASS \
    -out "$CRANVR_SIGN_STATE/identity.p12" -passout env:CRANVR_SIGN_KEYCHAIN_PASS

  security create-keychain -p "$CRANVR_SIGN_KEYCHAIN_PASS" "$CRANVR_SIGN_KEYCHAIN"
  security set-keychain-settings -ut 21600 "$CRANVR_SIGN_KEYCHAIN"
  security unlock-keychain -p "$CRANVR_SIGN_KEYCHAIN_PASS" "$CRANVR_SIGN_KEYCHAIN"
  local keep=("$CRANVR_SIGN_KEYCHAIN")
  while IFS= read -r existing; do
    [ -n "$existing" ] && keep+=("$existing")
  done < <(security list-keychains -d user | sed 's/^[[:space:]]*"//;s/"$//')
  security list-keychains -d user -s "${keep[@]}"

  local intermediate="$CRANVR_SIGN_STATE/DeveloperIDG2CA.cer"
  curl --proto '=https' --tlsv1.2 --retry 5 --retry-connrefused \
    --location --silent --show-error --fail --output "$intermediate" \
    https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer
  local expected=f16cd3c54c7f83cea4bf1a3e6a0819c8aaa8e4a1528fd144715f350643d2df3a
  test "$(shasum -a 256 "$intermediate" | cut -d' ' -f1)" = "$expected"
  security add-certificates -k "$CRANVR_SIGN_KEYCHAIN" "$intermediate"
  security import "$CRANVR_SIGN_STATE/identity.p12" -k "$CRANVR_SIGN_KEYCHAIN" \
    -P "$CRANVR_SIGN_KEYCHAIN_PASS" -T /usr/bin/codesign -f pkcs12
  security set-key-partition-list -S apple-tool:,apple:,codesign: \
    -s -k "$CRANVR_SIGN_KEYCHAIN_PASS" "$CRANVR_SIGN_KEYCHAIN" >/dev/null
  security find-certificate -a -Z -c 'Developer ID Certification Authority' "$CRANVR_SIGN_KEYCHAIN" \
    | grep -E 'SHA-256 hash:|"labl"'

  # A codeSign-only evaluation can fetch a missing issuer and conceal a broken
  # local chain. Check local certificates, then actually exercise the private key.
  security verify-cert -L -p basic -c "$CRANVR_SIGN_STATE/leaf.pem" -k "$CRANVR_SIGN_KEYCHAIN"
  local identity
  identity=$(openssl x509 -in "$CRANVR_SIGN_STATE/leaf.pem" -noout -fingerprint -sha1 | cut -d= -f2 | tr -d ':')
  security find-identity -v -p codesigning "$CRANVR_SIGN_KEYCHAIN" \
    | grep -F "$identity" | grep -F 'Developer ID Application:'
  cp /usr/bin/true "$CRANVR_SIGN_STATE/signing-probe"
  codesign --force --options runtime --timestamp --keychain "$CRANVR_SIGN_KEYCHAIN" \
    --sign "$identity" "$CRANVR_SIGN_STATE/signing-probe"
  codesign --verify --strict "$CRANVR_SIGN_STATE/signing-probe"
  printf 'CRANVR_SIGN_IDENTITY=%s\n' "$identity" >> "$GITHUB_ENV"
}

cleanup() {
  [ -n "${CRANVR_SIGN_STATE:-}" ] || return 0
  case "$CRANVR_SIGN_STATE" in
    "${RUNNER_TEMP:?}"/cranvr-developer-id.*) ;;
    *) echo 'Refusing to clean an unexpected signing directory' >&2; return 1 ;;
  esac
  local result=0
  if [ -f "${CRANVR_SIGN_KEYCHAIN:-}" ]; then
    local keep=()
    while IFS= read -r existing; do
      [ "$existing" = "$CRANVR_SIGN_KEYCHAIN" ] || keep+=("$existing")
    done < <(security list-keychains -d user | sed 's/^[[:space:]]*"//;s/"$//')
    if [ "${#keep[@]}" -gt 0 ]; then
      security list-keychains -d user -s "${keep[@]}" || result=1
    else
      security list-keychains -d user -s || result=1
    fi
    security delete-keychain "$CRANVR_SIGN_KEYCHAIN" || result=1
  fi
  rm -rf "$CRANVR_SIGN_STATE"
  return "$result"
}

case "${1:-}" in
  prepare) prepare ;;
  cleanup) cleanup ;;
  *) echo 'Usage: macos-signing.sh prepare|cleanup' >&2; exit 2 ;;
esac
