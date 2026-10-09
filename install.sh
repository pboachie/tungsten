#!/bin/sh
# Installs the tungsten CLI from a GitHub Release. POSIX sh, no sudo.
#
#   curl -fsSL https://raw.githubusercontent.com/pboachie/tungsten/main/install.sh | sh
#   sh install.sh [--version v0.1.0] [--dir DIR]
#
# Options (flags win over the environment):
#   --version, TUNGSTEN_VERSION   release tag to install (default: the latest release)
#   --dir, TUNGSTEN_INSTALL_DIR   target directory (default: $HOME/.local/bin)
#   TUNGSTEN_REPO                 owner/name of the repository (default: pboachie/tungsten)
#   TUNGSTEN_RELEASE_BASE_URL     directory URL laid out like GitHub's releases/download
#                                 (<base>/<tag>/<asset>), for mirrors and tests; needs a version
#   TUNGSTEN_REQUIRE_ATTESTATION  1: fail unless the `gh` CLI verifies the build attestation
#   TUNGSTEN_NO_ATTESTATION       1: do not look for an attestation at all
#
# The archive is checked against the release's SHA256SUMS before anything is
# installed. When the GitHub CLI (gh) is available the archive's build
# provenance attestation is verified too.
set -eu

say() { printf '%s\n' "install.sh: $*" >&2; }
die() { say "error: $*"; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

fetch() { # fetch URL DEST ; return 22 for "not found"
  if have curl; then
    curl -fsSL --retry 3 -o "$2" "$1" || { rc=$?; [ "$rc" -eq 22 ] && return 22; return 1; }
  elif have wget; then
    wget -q -O "$2" "$1" || { rc=$?; [ "$rc" -eq 8 ] && return 22; return 1; }
  else
    die "curl or wget is required"
  fi
}

sha256_of() {
  if have sha256sum; then sha256sum "$1" | cut -d ' ' -f 1
  elif have shasum; then shasum -a 256 "$1" | cut -d ' ' -f 1
  elif have openssl; then openssl dgst -sha256 "$1" | sed 's/^.*= *//'
  else die "sha256sum, shasum or openssl is required to verify the download"
  fi
}

target_triple() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "unsupported CPU architecture: $arch (releases cover x86_64 and aarch64)" ;;
  esac
  case "$os" in
    Linux) echo "$arch-unknown-linux-musl" ;;
    Darwin) echo "$arch-apple-darwin" ;;
    *) die "unsupported operating system: $os (on Windows use install.ps1)" ;;
  esac
}

latest_tag() {
  url="https://github.com/$repo/releases/latest"
  have curl || die "curl is required to find the latest release; pass --version instead"
  final=$(curl -fsSL --retry 3 -o /dev/null -w '%{url_effective}' "$url") \
    || die "no release found: $url does not exist yet. Releases are not published until v0.1.0; build from source with: cargo install --git https://github.com/$repo tungsten-cli"
  tag=${final##*/}
  case "$tag" in
    v[0-9]*) echo "$tag" ;;
    *) die "no release found at $url. Releases are not published until v0.1.0; build from source with: cargo install --git https://github.com/$repo tungsten-cli" ;;
  esac
}

main() {
  repo=${TUNGSTEN_REPO:-pboachie/tungsten}
  version=${TUNGSTEN_VERSION:-}
  dir=${TUNGSTEN_INSTALL_DIR:-}
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --version) [ "$#" -ge 2 ] || die "--version needs a value"; version=$2; shift 2 ;;
      --dir) [ "$#" -ge 2 ] || die "--dir needs a value"; dir=$2; shift 2 ;;
      -h | --help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; return 0 ;;
      *) die "unknown option: $1" ;;
    esac
  done
  if [ -z "$dir" ]; then
    [ -n "${HOME:-}" ] || die "HOME is not set; pass --dir"
    dir=$HOME/.local/bin
  fi

  base=${TUNGSTEN_RELEASE_BASE_URL:-https://github.com/$repo/releases/download}
  if [ -z "$version" ]; then
    [ -z "${TUNGSTEN_RELEASE_BASE_URL:-}" ] || die "TUNGSTEN_RELEASE_BASE_URL needs --version"
    version=$(latest_tag)
  fi
  case "$version" in v[0-9]*) ;; *) version=v$version ;; esac
  case "$version" in *[!A-Za-z0-9._+-]*) die "invalid version: $version" ;; esac

  target=$(target_triple)
  name="tungsten-${version#v}-$target"
  asset="$name.tar.gz"
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT INT TERM

  say "installing tungsten $version for $target"
  rc=0
  fetch "$base/$version/SHA256SUMS" "$work/SHA256SUMS" || rc=$?
  if [ "$rc" -eq 22 ]; then
    die "no release $version found at $base/$version/ (no SHA256SUMS). Releases are not published until v0.1.0; build from source with: cargo install --git https://github.com/$repo tungsten-cli"
  elif [ "$rc" -ne 0 ]; then
    die "could not download $base/$version/SHA256SUMS"
  fi
  rc=0
  fetch "$base/$version/$asset" "$work/$asset" || rc=$?
  if [ "$rc" -eq 22 ]; then
    die "release $version has no archive for $target ($asset)"
  elif [ "$rc" -ne 0 ]; then
    die "could not download $base/$version/$asset"
  fi

  expected=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1; exit }' "$work/SHA256SUMS")
  [ -n "$expected" ] || die "$asset is not listed in SHA256SUMS"
  actual=$(sha256_of "$work/$asset")
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset (expected $expected, got $actual); nothing was installed"
  say "checksum ok"

  verify_attestation "$work/$asset"

  tar -xzf "$work/$asset" -C "$work" "$name/tungsten" || die "unexpected archive layout in $asset"
  mkdir -p "$dir" || die "cannot create $dir; pass --dir with a writable directory"
  cp "$work/$name/tungsten" "$dir/.tungsten.$$" || die "cannot write to $dir"
  chmod 755 "$dir/.tungsten.$$"
  mv -f "$dir/.tungsten.$$" "$dir/tungsten"
  say "installed $dir/tungsten"
  case ":$PATH:" in
    *":$dir:"*) ;;
    *) say "note: $dir is not on your PATH; add it, for example: export PATH=\"$dir:\$PATH\"" ;;
  esac
}

verify_attestation() {
  [ "${TUNGSTEN_NO_ATTESTATION:-0}" = 1 ] && return 0
  if [ -n "${TUNGSTEN_RELEASE_BASE_URL:-}" ]; then
    say "custom release location: skipping the build attestation check"
    return 0
  fi
  if have gh; then
    gh attestation verify "$1" --repo "$repo" >/dev/null 2>&1 \
      || die "the build attestation of $1 could not be verified; nothing was installed"
    say "build attestation verified"
  elif [ "${TUNGSTEN_REQUIRE_ATTESTATION:-0}" = 1 ]; then
    die "TUNGSTEN_REQUIRE_ATTESTATION=1 but the gh CLI is not installed"
  else
    say "gh is not installed: build attestation not checked (checksum only)"
  fi
}

main "$@"
