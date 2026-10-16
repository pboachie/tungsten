#!/bin/sh
# Installs the tungsten CLI from a GitHub Release. POSIX sh, no sudo.
#
#   curl -fsSL https://raw.githubusercontent.com/pboachie/tungsten/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/pboachie/tungsten/main/install.sh | sh -s -- --init
#   sh install.sh [--version v0.1.2] [--dir DIR] [--init]
#
# Options (flags win over the environment):
#   --init                        after installing, run the guided `tungsten init` setup;
#                                 needs a terminal (it reads answers from /dev/tty)
#   --version, TUNGSTEN_VERSION   release tag to install (default: the latest release)
#   --dir, TUNGSTEN_INSTALL_DIR   target directory (default: $HOME/.local/bin)
#   TUNGSTEN_REPO                 owner/name of the repository (default: pboachie/tungsten)
#   TUNGSTEN_RELEASE_BASE_URL     directory URL laid out like GitHub's releases/download
#                                 (<base>/<tag>/<asset>), for mirrors and tests; needs a version
#   TUNGSTEN_REQUIRE_ATTESTATION  1: fail unless the `gh` CLI verifies the build attestation
#   TUNGSTEN_NO_ATTESTATION       1: do not look for an attestation at all
#                                 (cannot be combined with REQUIRE_ATTESTATION=1)
#   TUNGSTEN_LIBC                 Linux archive ABI: musl (default, static) or
#                                 gnu (dynamically linked; needs glibc 2.34+)
#
# The archive is checked against the release's SHA256SUMS before anything is
# installed. When the GitHub CLI (gh) is available the archive's build
# provenance attestation is verified too.
#
# Exit status: 0 installed (and, with --init, set up); 1 nothing installed or
# the install failed; 4 installed, but the --init setup was cancelled (or
# refused); 5 installed, but the --init setup failed.
set -eu

say() { printf '%s\n' "install.sh: $*" >&2; }
die() { say "error: $*"; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# The -gnu archives are dynamically linked and need glibc 2.34 or newer; the
# x86_64 and aarch64 builds share that one floor (the release pipeline enforces
# it on every built binary). The musl default is static and has no floor.
gnu_glibc_floor='2.34'

glibc_version() { # print the runtime glibc version, or nothing if undetectable
  ldd --version 2>/dev/null | awk 'NR == 1 { print $NF }'
}

version_lt() { # version_lt A B: succeed when version A is older than B
  awk -v a="$1" -v b="$2" 'BEGIN {
    na = split(a, A, ".")
    nb = split(b, B, ".")
    n = na > nb ? na : nb
    for (i = 1; i <= n; i++) {
      x = (i <= na ? A[i] : 0) + 0
      y = (i <= nb ? B[i] : 0) + 0
      if (x < y) exit 0
      if (x > y) exit 1
    }
    exit 1
  }'
}

check_gnu_glibc_floor() { # refuse a gnu install on a system below the floor
  if runtime_glibc=$(glibc_version) && [ -n "$runtime_glibc" ]; then
    if version_lt "$runtime_glibc" "$gnu_glibc_floor"; then
      die "the gnu archive needs glibc $gnu_glibc_floor or newer; this system reports $runtime_glibc. Keep the default musl build (unset TUNGSTEN_LIBC or set TUNGSTEN_LIBC=musl); it is static"
    fi
    say "glibc $runtime_glibc detected; the gnu archive needs $gnu_glibc_floor or newer"
  else
    say "could not detect the glibc version; the gnu archive needs glibc $gnu_glibc_floor or newer (the musl default is static and always works)"
  fi
}

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
    Linux)
      libc=${TUNGSTEN_LIBC:-musl}
      case "$libc" in musl | gnu) ;; *) die "unsupported Linux libc: $libc (use musl or gnu)" ;; esac
      echo "$arch-unknown-linux-$libc"
      ;;
    Darwin)
      [ -z "${TUNGSTEN_LIBC:-}" ] || die "TUNGSTEN_LIBC is only supported on Linux"
      echo "$arch-apple-darwin"
      ;;
    *) die "unsupported operating system: $os (on Windows use install.ps1)" ;;
  esac
}

latest_tag() {
  url="https://github.com/$repo/releases/latest"
  have curl || die "curl is required to find the latest release; pass --version instead"
  final=$(curl -fsSL --retry 3 -o /dev/null -w '%{url_effective}' "$url") \
    || die "no latest release; see https://github.com/$repo/releases; build from source with: cargo install --git https://github.com/$repo tungsten-cli"
  tag=${final##*/}
  case "$tag" in
    v[0-9]*) echo "$tag" ;;
    *) die "no latest release; see https://github.com/$repo/releases; build from source with: cargo install --git https://github.com/$repo tungsten-cli" ;;
  esac
}

main() {
  repo=${TUNGSTEN_REPO:-pboachie/tungsten}
  version=${TUNGSTEN_VERSION:-}
  dir=${TUNGSTEN_INSTALL_DIR:-}
  launch_init=0
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --init) launch_init=1; shift ;;
      --version) [ "$#" -ge 2 ] || die "--version needs a value"; version=$2; shift 2 ;;
      --dir) [ "$#" -ge 2 ] || die "--dir needs a value"; dir=$2; shift 2 ;;
      -h | --help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; return 0 ;;
      *) die "unknown option: $1" ;;
    esac
  done
  if [ "$launch_init" -eq 1 ]; then
    (test -t 0 </dev/tty) 2>/dev/null || die "--init requires a controlling terminal; install without --init then run tungsten init interactively"
  fi
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
  case "$target" in *-linux-gnu) check_gnu_glibc_floor ;; esac
  name="tungsten-${version#v}-$target"
  asset="$name.tar.gz"
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT INT TERM

  say "installing tungsten $version for $target"
  rc=0
  fetch "$base/$version/SHA256SUMS" "$work/SHA256SUMS" || rc=$?
  if [ "$rc" -eq 22 ]; then
    die "no release $version (no SHA256SUMS); see https://github.com/$repo/releases; build from source with: cargo install --git https://github.com/$repo tungsten-cli"
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
  if [ "$launch_init" -eq 1 ]; then
    setup=0
    "$dir/tungsten" init --interactive </dev/tty || setup=$?
    case "$setup" in
      0) ;;
      4)
        say "setup cancelled (or refused; see above): tungsten is installed; run $dir/tungsten init when ready"
        exit 4
        ;;
      *)
        say "setup failed (tungsten init exited $setup): tungsten is installed; correct the problem and run $dir/tungsten init again"
        exit 5
        ;;
    esac
  fi
}

verify_attestation() {
  if [ "${TUNGSTEN_NO_ATTESTATION:-0}" = 1 ]; then
    [ "${TUNGSTEN_REQUIRE_ATTESTATION:-0}" != 1 ] \
      || die "TUNGSTEN_NO_ATTESTATION=1 conflicts with TUNGSTEN_REQUIRE_ATTESTATION=1"
    say "checksum verified; attestation not checked (TUNGSTEN_NO_ATTESTATION=1)"
    return 0
  fi
  if [ -n "${TUNGSTEN_RELEASE_BASE_URL:-}" ]; then
    [ "${TUNGSTEN_REQUIRE_ATTESTATION:-0}" != 1 ] \
      || die "TUNGSTEN_REQUIRE_ATTESTATION=1 cannot verify a custom release location"
    say "custom release location: skipping the build attestation check"
    return 0
  fi
  reason='gh is not installed'
  if have gh && gh attestation --help >"$work/attestation-help.log" 2>&1; then
    gh --version >"$work/gh-version.log" 2>&1 \
      || die "cannot determine GitHub CLI version; update to 2.102.0 or newer"
    awk 'NR == 1 { split($3,v,"."); ok = ($1 == "gh" && $2 == "version" && $3 ~ /^[0-9]+\.[0-9]+\.[0-9]+$/ && (v[1] > 2 || (v[1] == 2 && v[2] >= 102))) } END { exit !ok }' "$work/gh-version.log" \
      || die "GitHub CLI 2.102.0 or newer is required for exact workflow attestation verification"
    if ! gh attestation verify "$1" --repo "$repo" --signer-workflow "$repo/.github/workflows/release.yml" >"$work/attestation-out.log" 2>"$work/attestation-error.log"; then
      tail -n 8 "$work/attestation-error.log" >&2
      die "the build attestation could not be verified; nothing was installed"
    fi
    say "build attestation verified"
  else
    if have gh; then
      reason='gh attestation is unavailable; update GitHub CLI (2.49 or newer)'
      tail -n 8 "$work/attestation-help.log" >&2
    fi
    [ "${TUNGSTEN_REQUIRE_ATTESTATION:-0}" != 1 ] \
      || die "TUNGSTEN_REQUIRE_ATTESTATION=1 but $reason"
    say "checksum verified; attestation not checked ($reason)"
  fi
}

main "$@"
