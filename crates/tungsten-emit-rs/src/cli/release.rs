// SPDX-License-Identifier: AGPL-3.0-only
//! `.github/workflows/release.yml`: static binaries of the CLI for Linux
//! (musl), macOS and Windows, attached to a GitHub release with SHA-256 sums.
//!
//! Only `actions/checkout` is used (pinned by commit SHA, as in the
//! repository's own ci.yml); everything else is `rustup`, `cargo` and the
//! `gh` CLI that the runners provide. The token is read-only except in the
//! jobs that write to the release, and nothing from the event is
//! interpolated into a script: it goes through the environment.

/// `actions/checkout`, the SHA ci.yml of the tungsten repository pins.
pub(crate) const CHECKOUT: &str =
    "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1";

/// The build matrix: (target, runner, archive extension).
pub(crate) const TARGETS: &[(&str, &str, &str)] = &[
    ("x86_64-unknown-linux-musl", "ubuntu-latest", "tar.gz"),
    ("aarch64-unknown-linux-musl", "ubuntu-24.04-arm", "tar.gz"),
    ("x86_64-apple-darwin", "macos-latest", "tar.gz"),
    ("aarch64-apple-darwin", "macos-latest", "tar.gz"),
    ("x86_64-pc-windows-msvc", "windows-latest", "zip"),
];

pub(crate) fn workflow(header: &str, bin: &str, package: &str) -> String {
    let mut matrix = String::new();
    for (target, os, archive) in TARGETS {
        matrix.push_str(&format!(
            "          - {{ target: {target}, os: {os}, archive: {archive} }}\n"
        ));
    }
    format!(
        r#"{header}
#
# Builds the command-line client for Linux (musl, static), macOS and Windows
# and attaches an archive and its SHA-256 sum per target to a GitHub release,
# then a combined SHA256SUMS. Run it by pushing a tag such as v1.2.3, or from
# the Actions tab with the name of an existing tag. The generated workspace
# must be the root of the repository.
name: release

on:
  push:
    tags: ["v*"]
  workflow_dispatch:
    inputs:
      tag:
        description: "Existing tag to build and release (v1.2.3)"
        required: true
        type: string

permissions:
  contents: read

concurrency:
  group: release-${{{{ github.event.inputs.tag || github.ref_name }}}}
  cancel-in-progress: false

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: "0"
  BIN: {bin}
  PACKAGE: {package}

jobs:
  prepare:
    name: draft release
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: write
    outputs:
      tag: ${{{{ steps.tag.outputs.tag }}}}
    env:
      GH_TOKEN: ${{{{ github.token }}}}
      GH_REPO: ${{{{ github.repository }}}}
    steps:
      - name: Check the tag
        id: tag
        env:
          EVENT: ${{{{ github.event_name }}}}
          INPUT_TAG: ${{{{ github.event.inputs.tag }}}}
          REF_NAME: ${{{{ github.ref_name }}}}
        run: |
          tag="$REF_NAME"
          if [ "$EVENT" = workflow_dispatch ]; then tag="$INPUT_TAG"; fi
          if ! printf '%s' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([-+.][0-9A-Za-z.-]+)*$'; then
            echo "the tag must look like v1.2.3, got: $tag" >&2
            exit 1
          fi
          gh api "repos/$GH_REPO/git/ref/tags/$tag" >/dev/null \
            || {{ echo "the tag $tag does not exist" >&2; exit 1; }}
          echo "tag=$tag" >> "$GITHUB_OUTPUT"
      - name: Create the draft release when it does not exist
        env:
          TAG: ${{{{ steps.tag.outputs.tag }}}}
        run: |
          gh release view "$TAG" >/dev/null 2>&1 \
            || gh release create "$TAG" --draft --verify-tag --title "$TAG" --notes ""

  build:
    name: build ${{{{ matrix.target }}}}
    needs: prepare
    runs-on: ${{{{ matrix.os }}}}
    timeout-minutes: 45
    permissions:
      contents: write
    strategy:
      fail-fast: false
      matrix:
        include:
{matrix}    env:
      TAG: ${{{{ needs.prepare.outputs.tag }}}}
      TARGET: ${{{{ matrix.target }}}}
      GH_TOKEN: ${{{{ github.token }}}}
      GH_REPO: ${{{{ github.repository }}}}
    steps:
      - uses: {checkout}
        with:
          ref: ${{{{ needs.prepare.outputs.tag }}}}
          persist-credentials: false
      - name: Install musl tools
        if: contains(matrix.target, 'linux-musl')
        run: sudo apt-get update && sudo apt-get install -y --no-install-recommends musl-tools
      - name: Install the Rust toolchain
        shell: bash
        run: |
          rustup toolchain install stable --profile minimal --target "$TARGET"
          rustup default stable
          rustc --version
      - name: Build a static, stripped binary
        shell: bash
        env:
          CARGO_PROFILE_RELEASE_STRIP: symbols
          RUSTFLAGS: ${{{{ contains(matrix.target, 'windows') && '-C target-feature=+crt-static' || '' }}}}
        run: |
          if [ "${{RUNNER_OS}}" = Linux ]; then
            export "CC_${{TARGET//-/_}}=musl-gcc"
          fi
          cargo build --release --package "$PACKAGE" --target "$TARGET"
      - name: Archive, sum and upload
        shell: bash
        env:
          ARCHIVE: ${{{{ matrix.archive }}}}
        run: |
          name="$BIN-$TAG-$TARGET"
          exe="$BIN"
          if [ "${{RUNNER_OS}}" = Windows ]; then exe="$BIN.exe"; fi
          mkdir -p "dist/$name"
          cp "target/$TARGET/release/$exe" "dist/$name/"
          cd dist
          if [ "$ARCHIVE" = zip ]; then
            7z a "$name.zip" "$name" >/dev/null
          else
            tar -czf "$name.tar.gz" "$name"
          fi
          file="$name.$ARCHIVE"
          if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "$file" > "$file.sha256"
          else
            shasum -a 256 "$file" > "$file.sha256"
          fi
          gh release upload "$TAG" "$file" "$file.sha256" --clobber

  publish:
    name: publish release
    needs: [prepare, build]
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: write
    env:
      TAG: ${{{{ needs.prepare.outputs.tag }}}}
      GH_TOKEN: ${{{{ github.token }}}}
      GH_REPO: ${{{{ github.repository }}}}
    steps:
      - name: Combine the sums and publish the release
        run: |
          mkdir sums
          cd sums
          gh release download "$TAG" --pattern '*.sha256'
          cat ./*.sha256 | sort -k2 > SHA256SUMS
          gh release upload "$TAG" SHA256SUMS --clobber
          gh release edit "$TAG" --draft=false
"#,
        checkout = CHECKOUT,
    )
}
