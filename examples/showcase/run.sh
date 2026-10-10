#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
#
# Reproduce examples/showcase/README.md: download the pinned public OpenAPI
# descriptions listed in specs.lock into .cache/specs (hash-verified), run
# tungsten on each project in apis/, and rewrite the generated blocks of the
# README. No API is called; the specs come from GitHub raw URLs at pinned commits.
#
#   ./run.sh                   download, run, rewrite the README blocks
#   ./run.sh --only ID         the same for one API (stripe, github, twilio, ...)
#   ./run.sh --check           run, and fail if the committed README differs
#   ./run.sh --update-lock     re-download every spec and rewrite its hash in specs.lock
#   ./run.sh --offline         never download: use .cache/specs as it is (hashes still checked)
#
# The tungsten binary is $TUNGSTEN, else `tungsten` on PATH.
# Exit status: 0 ok, 1 --check found a difference, 2 a spec could not be downloaded
# or tungsten was not found, 3 a hash mismatch or an internal failure.
set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cache=$here/.cache
mode=write
only=
offline=0

while [ $# -gt 0 ]; do
    case $1 in
        --check) mode=check ;;
        --update-lock) mode=update-lock ;;
        --offline) offline=1 ;;
        --only)
            [ $# -ge 2 ] || { echo "run.sh: --only needs an API id" >&2; exit 3; }
            only=$2
            shift
            ;;
        -h | --help)
            awk 'NR >= 4 && /^#/ { sub(/^# ?/, ""); print; next } NR >= 4 { exit }' "$0"
            exit 0
            ;;
        *) echo "run.sh: unknown option $1" >&2; exit 3 ;;
    esac
    shift
done

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        echo "run.sh: need sha256sum or shasum" >&2
        exit 3
    fi
}

download() { # url file
    command -v curl >/dev/null 2>&1 || { echo "run.sh: curl is needed to download the specs" >&2; exit 2; }
    curl --fail --silent --show-error --location --retry 3 --output "$2" "$1" || return 1
}

specs=$cache/specs
mkdir -p "$specs"

# Entries of specs.lock: <sha256>  <file>  <url>
lock_entries() { grep -v '^[[:space:]]*#' "$here/specs.lock" | grep -v '^[[:space:]]*$'; }

if [ "$mode" = update-lock ]; then
    tmp=$cache/specs.lock.new
    grep '^[[:space:]]*#' "$here/specs.lock" >"$tmp" || true
    lock_entries | while read -r _sha file url; do
        download "$url" "$specs/$file.part" || { echo "run.sh: cannot download $url" >&2; exit 2; }
        mv "$specs/$file.part" "$specs/$file"
        printf '%s  %s  %s\n' "$(sha256_of "$specs/$file")" "$file" "$url" >>"$tmp"
    done
    mv "$tmp" "$here/specs.lock"
    echo "specs.lock rewritten; review the diff"
    exit 0
fi

tungsten=${TUNGSTEN:-tungsten}
if ! command -v "$tungsten" >/dev/null 2>&1; then
    echo "run.sh: tungsten not found; install it or set TUNGSTEN=/path/to/tungsten" >&2
    exit 2
fi
command -v python3 >/dev/null 2>&1 || { echo "run.sh: python3 is needed to render the README" >&2; exit 2; }

lock_entries | while read -r sha file url; do
    if [ -f "$specs/$file" ] && [ "$(sha256_of "$specs/$file")" = "$sha" ]; then
        continue
    fi
    if [ "$offline" = 1 ]; then
        echo "run.sh: $file is missing or stale in the cache and --offline was given" >&2
        exit 2
    fi
    echo "downloading $file"
    if ! download "$url" "$specs/$file.part"; then
        rm -f "$specs/$file.part"
        echo "run.sh: cannot download $url" >&2
        exit 2
    fi
    got=$(sha256_of "$specs/$file.part")
    if [ "$got" != "$sha" ]; then
        rm -f "$specs/$file.part"
        echo "run.sh: $file has SHA-256 $got, specs.lock expects $sha" >&2
        exit 3
    fi
    mv "$specs/$file.part" "$specs/$file"
done

ids=$(for d in "$here"/apis/*/; do basename "$d"; done)
if [ -n "$only" ]; then
    if printf '%s\n' "$ids" | grep -qx -- "$only"; then
        ids=$only
    else
        echo "run.sh: unknown API '$only' (known: $(printf '%s ' $ids))" >&2
        exit 3
    fi
fi

cd "$here"
for id in $ids; do
    echo "== $id"
    data=$cache/data/$id
    rm -rf "$data" "$cache/out/$id"
    mkdir -p "$data"
    # A project with errors still has a report worth rendering, so only the
    # outputs are required to exist; `generate` must succeed.
    "$tungsten" check "apis/$id" >"$data/check.txt" 2>/dev/null || true
    "$tungsten" --json check "apis/$id" >"$data/check.json" 2>/dev/null || true
    "$tungsten" --json report "apis/$id" >"$data/report.json" 2>/dev/null || true
    "$tungsten" generate "apis/$id" >/dev/null 2>&1 || { echo "run.sh: generate failed for $id" >&2; exit 3; }
done

set -- --data "$cache/data" --out "$cache/out"
[ -z "$only" ] || set -- "$@" --only "$only"
if [ "$mode" = check ]; then
    python3 -I "$here/render.py" --check "$@"
else
    python3 -I "$here/render.py" --write "$@"
fi
