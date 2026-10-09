#!/bin/sh
# Packages one built binary of the tungsten CLI as a release archive plus its
# SHA-256 sum. Used by .github/workflows/release.yml for every target.
#
#   package.sh <version> <target> <binary> <outdir>
#
# Writes <outdir>/tungsten-<version>-<target>.tar.gz (.zip for Windows targets)
# and <outdir>/<archive>.sha256 in `sha256sum` format. The archive holds one
# directory, tungsten-<version>-<target>/, with the binary, LICENSE and
# README.md. File times come from SOURCE_DATE_EPOCH (default: the HEAD commit)
# and owners are zeroed, so the same binary always gives the same archive.
set -eu

[ "$#" -eq 4 ] || { echo "usage: package.sh <version> <target> <binary> <outdir>" >&2; exit 2; }
version=$1 target=$2 binary=$3 outdir=$4

[ -f "$binary" ] || { echo "package.sh: no such binary: $binary" >&2; exit 1; }
root=$(cd "$(dirname "$0")/../.." && pwd)
name="tungsten-$version-$target"
case "$target" in
  *windows*) ext=zip exe=tungsten.exe ;;
  *) ext=tar.gz exe=tungsten ;;
esac

if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
  SOURCE_DATE_EPOCH=$(git -C "$root" log -1 --format=%ct 2>/dev/null || echo 0)
fi
export SOURCE_DATE_EPOCH

mkdir -p "$outdir"
outdir=$(cd "$outdir" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT INT TERM
mkdir "$stage/$name"
cp "$binary" "$stage/$name/$exe"
chmod 755 "$stage/$name/$exe"
cp "$root/LICENSE" "$root/README.md" "$stage/$name/"

archive="$name.$ext"
rm -f "$outdir/$archive"
(
  cd "$stage"
  # Normalise times so the archive does not depend on when it was built.
  find "$name" -exec touch -d "@$SOURCE_DATE_EPOCH" {} + 2>/dev/null \
    || find "$name" -exec touch -t "$(date -u -r "$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S)" {} +
  if [ "$ext" = zip ]; then
    if command -v 7z >/dev/null 2>&1; then
      7z a -tzip -mtc=off "$outdir/$archive" "$name" >/dev/null
    else
      zip -qr -X "$outdir/$archive" "$name"
    fi
  elif tar --version 2>/dev/null | grep -q 'GNU tar'; then
    tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$SOURCE_DATE_EPOCH" \
      -cf - "$name" | gzip -n -9 > "$outdir/$archive"
  else
    tar --uid 0 --gid 0 --numeric-owner -cf - "$name" | gzip -n -9 > "$outdir/$archive"
  fi
)

cd "$outdir"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$archive" > "$archive.sha256"
else
  shasum -a 256 "$archive" > "$archive.sha256"
fi
echo "$outdir/$archive"
