#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Generate package-manager manifests and container contexts from checked assets."""
import argparse
import gzip
import hashlib
import io
import json
import re
import tarfile
from pathlib import Path

REPO = "pboachie/tungsten"
BASE_TARGETS = (
    "x86_64-apple-darwin", "aarch64-apple-darwin",
    "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
)


def generate(bundle: Path, output: Path, tag: str) -> None:
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", tag):
        raise ValueError("tag must be vX.Y.Z or a semver prerelease")
    version = tag[1:]
    if output.exists() and any(output.iterdir()):
        raise ValueError("output directory must be empty")
    sums = {}
    for line in (bundle / "SHA256SUMS").read_text().splitlines():
        match = re.fullmatch(r"([0-9a-fA-F]{64})\s+\*?([^/\\]+)", line)
        if not match:
            raise ValueError("invalid checksum entry")
        digest, name = match.groups()
        if name in (".", "..") or name in sums:
            raise ValueError("invalid or duplicate checksum filename")
        path = bundle / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"missing regular release asset: {name}")
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest.lower():
            raise ValueError(f"checksum mismatch: {name}")
        sums[name] = digest.lower()

    def asset(target):
        ext = "zip" if "windows" in target else "tar.gz"
        name = f"tungsten-{version}-{target}.{ext}"
        if name not in sums:
            raise ValueError(f"required release asset is not checksummed: {name}")
        return name, f"https://github.com/{REPO}/releases/download/{tag}/{name}", sums[name]

    for target in BASE_TARGETS:
        asset(target)

    # Validate tar members before writing any output. Never extract arbitrary paths.
    binaries = {}
    license_text = None
    for arch, target in (("amd64", "x86_64-unknown-linux-musl"), ("arm64", "aarch64-unknown-linux-musl")):
        name, _, _ = asset(target)
        expected = f"tungsten-{version}-{target}/tungsten"
        with tarfile.open(bundle / name, "r:gz") as archive:
            members = [m for m in archive.getmembers() if m.name == expected]
            if len(members) != 1 or not members[0].isfile() or members[0].size > 256 * 1024 * 1024:
                raise ValueError(f"invalid CLI archive layout: {name}")
            binaries[arch] = archive.extractfile(members[0]).read()
            license_members = [m for m in archive.getmembers() if m.name == expected.rsplit("/", 1)[0] + "/LICENSE"]
            if len(license_members) != 1 or not license_members[0].isfile() or license_members[0].size > 256 * 1024:
                raise ValueError(f"invalid license layout: {name}")
            content = archive.extractfile(license_members[0]).read()
            if license_text is not None and content != license_text:
                raise ValueError("release architectures have different licenses")
            license_text = content

    lines = [
        "class Tungsten < Formula", '  desc "SDK generator and MCP compiler"',
        f'  homepage "https://github.com/{REPO}"', f'  version "{version}"',
        '  license "AGPL-3.0-only"',
    ]
    for os, suffix in (("macos", "apple-darwin"), ("linux", "unknown-linux-musl")):
        lines.append(f"  on_{os} do")
        for cpu, arch in (("arm", "aarch64"), ("intel", "x86_64")):
            _, url, digest = asset(f"{arch}-{suffix}")
            lines.extend([f"    on_{cpu} do", f'      url "{url}"', f'      sha256 "{digest}"', "    end"])
        lines.append("  end")
    lines.extend(["", "  def install", '    bin.install "tungsten"', "  end", "", "  test do", '    assert_match version.to_s, shell_output("#{bin}/tungsten --version")', "  end", "end", ""])

    package_id = "Elysra.Tungsten"
    shared = {"PackageIdentifier": package_id, "PackageVersion": version}
    installers = []
    for arch, target in (("x64", "x86_64-pc-windows-msvc"), ("arm64", "aarch64-pc-windows-msvc")):
        name = f"tungsten-{version}-{target}.zip"
        if arch == "arm64" and name not in sums:
            continue  # Older releases published x64 only.
        _, url, digest = asset(target)
        installers.append({"Architecture": arch, "InstallerUrl": url, "InstallerSha256": digest.upper(),
                           "NestedInstallerFiles": [{"RelativeFilePath": f"tungsten-{version}-{target}/tungsten.exe", "PortableCommandAlias": "tungsten"}]})
    manifests = {
        "": {**shared, "DefaultLocale": "en-US", "ManifestType": "version", "ManifestVersion": "1.10.0"},
        ".installer": {**shared, "InstallerType": "zip", "NestedInstallerType": "portable", "Installers": installers,
                       "ManifestType": "installer", "ManifestVersion": "1.10.0"},
        ".locale.en-US": {**shared, "PackageLocale": "en-US", "Publisher": "Elysra", "PackageName": "Tungsten",
                          "License": "AGPL-3.0-only", "LicenseUrl": f"https://github.com/{REPO}/blob/{tag}/LICENSE",
                          "ShortDescription": "SDK generator and MCP compiler", "PackageUrl": f"https://github.com/{REPO}",
                          "ManifestType": "defaultLocale", "ManifestVersion": "1.10.0"},
    }
    output.mkdir(parents=True, exist_ok=True)
    (output / "tungsten.rb").write_text("\n".join(lines))
    winget = output / "winget" / "manifests" / "e" / "Elysra" / "Tungsten" / version
    winget.mkdir(parents=True, exist_ok=True)
    for suffix, document in manifests.items():
        # JSON is a YAML subset accepted by the winget YAML parser.
        (winget / f"{package_id}{suffix}.yaml").write_text(json.dumps(document, indent=2) + "\n")
    # Stable bytes let the post-release consumer compare its regenerated
    # manifests with the archive sealed into the initial release bundle.
    with (output / "winget-manifests.tar.gz").open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for path in sorted((output / "winget").rglob("*.yaml")):
                    content = path.read_bytes()
                    info = tarfile.TarInfo(path.relative_to(output / "winget").as_posix())
                    info.size = len(content)
                    info.mode = 0o644
                    archive.addfile(info, io.BytesIO(content))
    for arch, binary in binaries.items():
        directory = output / "container" / f"linux-{arch}"
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / "tungsten"
        path.write_bytes(binary)
        path.chmod(0o755)
    workspace = output / "container" / "workspace"
    workspace.mkdir()
    (workspace / ".keep").write_text("")
    (output / "container" / "LICENSE").write_bytes(license_text)
    checksums = []
    for path in sorted(output.rglob("*")):
        if path.is_file() and path.name != "DISTRIBUTION_SHA256SUMS":
            checksums.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(output).as_posix()}")
    (output / "DISTRIBUTION_SHA256SUMS").write_text("\n".join(checksums) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--tag", required=True)
    args = parser.parse_args()
    generate(args.bundle, args.output, args.tag)
