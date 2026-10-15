#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Build or verify frozen native archives. Never authenticates or publishes."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import re
import shutil
import stat
import struct
import subprocess
import tarfile
import tomllib
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[2]
PACKAGES = {
    "java": "tungsten-runtime",
    "csharp": "Tungsten.Runtime",
    "ruby": "tungsten-runtime",
}
SEMVER = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
LIMIT = 128 * 1024 * 1024


def release_version(version):
    match = re.fullmatch(SEMVER, version)
    if not match or any(
        p.isdigit() and len(p) > 1 and p.startswith("0")
        for p in (match[4] or "").split(".")
    ):
        raise ValueError("expected a strict release version")
    return version


def run(args, cwd):
    subprocess.run([str(arg) for arg in args], cwd=cwd, check=True)


def source_commit():
    commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("source checkout has no exact commit")
    return commit


def regular(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > LIMIT:
        raise ValueError("expected a bounded regular file: " + path.name)
    return path.read_bytes()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON inventory field")
        result[key] = value
    return result


def safe_name(name):
    if (
        not name
        or "\\" in name
        or ":" in name
        or "\x00" in name
        or name.startswith("/")
    ):
        raise ValueError("unsafe archive member path")
    parts = name.rstrip("/").split("/")
    if any(part in ("", ".", "..") for part in parts):
        raise ValueError("unsafe archive member path")
    return str(PurePosixPath(*parts))


def zip_files(path):
    result, seen, size = {}, set(), 0
    with zipfile.ZipFile(io.BytesIO(regular(path))) as archive:
        for member in archive.infolist():
            name = safe_name(member.filename)
            mode = stat.S_IFMT(member.external_attr >> 16)
            if (
                name in seen
                or mode not in (0, stat.S_IFREG, stat.S_IFDIR)
                or member.flag_bits & 1
            ):
                raise ValueError("duplicate, encrypted or non-regular ZIP member")
            seen.add(name)
            size += member.file_size
            if size > LIMIT:
                raise ValueError("expanded archive exceeds the size limit")
            if not member.is_dir():
                result[name] = archive.read(member)
    return result


def tar_files(data, mode):
    result, seen, size = {}, set(), 0
    with tarfile.open(fileobj=io.BytesIO(data), mode=mode) as archive:
        for member in archive:
            name = safe_name(member.name)
            if name in seen or not (member.isfile() or member.isdir()):
                raise ValueError("duplicate or non-regular tar member")
            seen.add(name)
            size += member.size
            if size > LIMIT:
                raise ValueError("expanded archive exceeds the size limit")
            if member.isfile():
                result[name] = archive.extractfile(member).read()
    return result


def source_files(package):
    source = ROOT / "runtimes" / package
    pattern, manifest = {
        "java": ("src/main/java/**/*.java", "pom.xml"),
        "csharp": ("src/**/*.cs", "Tungsten.Runtime.csproj"),
        "ruby": ("lib/**/*.rb", "tungsten-runtime.gemspec"),
    }[package]
    paths = sorted(source.glob(pattern)) + [
        source / manifest,
        source / "README.md",
        source / "LICENSE",
    ]
    if package == "csharp":
        paths.append(ROOT / "assets/tungsten-icon.png")
    return {path.relative_to(ROOT).as_posix(): regular(path) for path in paths}


def source_digest(package):
    digest = hashlib.sha256()
    for name, content in sorted(source_files(package).items()):
        digest.update(name.encode() + b"\0" + hashlib.sha256(content).digest())
    return digest.hexdigest()


def xml(data):
    if b"<!DOCTYPE" in data.upper() or b"<!ENTITY" in data.upper():
        raise ValueError("unexpected XML declaration")
    return ET.fromstring(data)


def child(root, path):
    namespace = root.tag.split("}")[0] + "}" if "}" in root.tag else ""
    return root.find("/".join(namespace + part for part in path.split("/")))


def text(root, path):
    element = child(root, path)
    return element.text if element is not None else None


def check_pom(data, version):
    pom = xml(data)
    expected = {
        "groupId": "io.github.pboachie.tungsten",
        "artifactId": PACKAGES["java"],
        "version": version,
        "packaging": "jar",
        "licenses/license/name": "Apache-2.0",
        "licenses/license/url": "https://www.apache.org/licenses/LICENSE-2.0",
        "scm/url": "https://github.com/pboachie/tungsten",
        "scm/connection": "scm:git:https://github.com/pboachie/tungsten.git",
        "scm/developerConnection": "scm:git:ssh://git@github.com/pboachie/tungsten.git",
        "scm/tag": "v" + version,
        "developers/developer/id": "pboachie",
    }
    if any(text(pom, key) != value for key, value in expected.items()):
        raise ValueError("incorrect Maven identity, license, SCM or developer metadata")
    if any(
        not text(pom, f)
        for f in ("name", "description", "url", "developers/developer/name")
    ):
        raise ValueError("missing Maven publication metadata")


def check_java(directory, version):
    basename = f"tungsten-runtime-{version}"
    pom = regular(directory / (basename + ".pom"))
    check_pom(pom, version)
    if pom != regular(ROOT / "runtimes/java/pom.xml"):
        raise ValueError("published Maven POM differs from source")
    binary = zip_files(directory / (basename + ".jar"))
    sources = zip_files(directory / (basename + "-sources.jar"))
    docs = zip_files(directory / (basename + "-javadoc.jar"))
    for files in (binary, sources):
        for name in ("LICENSE", "README.md"):
            if files.get("META-INF/" + name) != regular(ROOT / "runtimes/java" / name):
                raise ValueError("Java archive notices differ from source")
    source = ROOT / "runtimes/java/src/main/java"
    expected = {
        p.relative_to(source).as_posix(): regular(p)
        for p in sorted(source.rglob("*.java"))
    }
    if (
        not expected
        or {n: v for n, v in sources.items() if n.endswith(".java")} != expected
    ):
        raise ValueError("Java source archive differs from source")
    classes = {name for name in binary if name.endswith(".class")}
    if not classes or not all(
        name[:-5] + ".class" in classes
        for name in expected
        if name != "module-info.java"
    ):
        raise ValueError("Java archive has an incomplete class inventory")
    for name in classes:
        content = binary[name]
        if (
            not content.startswith(b"\xca\xfe\xba\xbe")
            or len(content) < 8
            or struct.unpack(">H", content[6:8])[0] != 61
        ):
            raise ValueError("Java archive does not contain Java 17 bytecode")
        if name.split("$", 1)[0].removesuffix(".class") + ".java" not in expected:
            raise ValueError("Java archive contains an unexpected class")
    embedded = "META-INF/maven/io.github.pboachie.tungsten/tungsten-runtime/pom.xml"
    if binary.get(embedded) != pom:
        raise ValueError("Java archive embeds an unexpected POM")
    properties = binary.get(
        embedded.removesuffix("pom.xml") + "pom.properties", b""
    ).decode("utf-8")
    for field, value in (
        ("groupId", "io.github.pboachie.tungsten"),
        ("artifactId", "tungsten-runtime"),
        ("version", version),
    ):
        if not re.search(r"(?m)^" + field + "=" + re.escape(value) + r"$", properties):
            raise ValueError("Java archive properties have incorrect identity")
    if not docs.get("index.html") or not any(
        name.endswith("ClientCore.html") for name in docs
    ):
        raise ValueError("Java documentation archive is incomplete")


def check_csharp(directory, version, commit):
    files = zip_files(directory / f"Tungsten.Runtime.{version}.nupkg")
    core_properties = {
        name
        for name in files
        if re.fullmatch(
            r"package/services/metadata/core-properties/[^/]+\.psmdcp", name
        )
    }
    expected_files = {
        "_rels/.rels",
        "[Content_Types].xml",
        "Tungsten.Runtime.nuspec",
        "LICENSE",
        "README.md",
        "lib/net10.0/Tungsten.Runtime.dll",
        "tungsten-icon.png",
    }
    if len(core_properties) != 1 or set(files) != expected_files | core_properties:
        raise ValueError("NuGet package contains unexpected files")
    metadata = xml(files["Tungsten.Runtime.nuspec"])
    expected = {
        "id": PACKAGES["csharp"],
        "version": version,
        "license": "Apache-2.0",
        "readme": "README.md",
        "icon": "tungsten-icon.png",
    }
    if any(
        text(metadata, "metadata/" + key) != value for key, value in expected.items()
    ):
        raise ValueError(
            "NuGet identity, license, README or icon metadata is incorrect"
        )
    license_node = child(metadata, "metadata/license")
    repository = child(metadata, "metadata/repository")
    if (
        license_node.get("type") != "expression"
        or repository is None
        or repository.get("type") != "git"
        or repository.get("url") != "https://github.com/pboachie/tungsten"
        or repository.get("commit") != commit
    ):
        raise ValueError("NuGet license or immutable repository metadata is incorrect")
    for name in ("README.md", "LICENSE"):
        if files.get(name) != regular(ROOT / "runtimes/csharp" / name):
            raise ValueError("NuGet notices differ from source")
    icon = regular(ROOT / "assets/tungsten-icon.png")
    if (
        files.get("tungsten-icon.png") != icon
        or icon[:8] != b"\x89PNG\r\n\x1a\n"
        or icon[12:16] != b"IHDR"
        or struct.unpack(">II", icon[16:24]) != (128, 128)
    ):
        raise ValueError("NuGet icon must embed the checked 128px PNG")
    assemblies = {
        name for name in files if name.startswith("lib/") and name.endswith(".dll")
    }
    if assemblies != {"lib/net10.0/Tungsten.Runtime.dll"} or not files[
        next(iter(assemblies))
    ].startswith(b"MZ"):
        raise ValueError("NuGet package has an unexpected assembly target")


def check_ruby(directory, version):
    gem = directory / f"tungsten-runtime-{version}.gem"
    outer = tar_files(regular(gem), "r:")
    if set(outer) != {"metadata.gz", "data.tar.gz", "checksums.yaml.gz"}:
        raise ValueError("Ruby archive has unexpected outer members")
    files = tar_files(outer["data.tar.gz"], "r:gz")
    source = ROOT / "runtimes/ruby"
    expected = {
        p.relative_to(source).as_posix(): regular(p) for p in source.glob("lib/**/*.rb")
    }
    expected.update({name: regular(source / name) for name in ("README.md", "LICENSE")})
    if files != expected:
        raise ValueError("Ruby archive content differs from source")
    command = """p = Gem::Package.new(ARGV[0]); p.verify; s = p.spec
puts JSON.generate(name: s.name, version: s.version.to_s, release_version: Gem::Version.new(ARGV[1]).to_s, licenses: s.licenses,
 ruby: s.required_ruby_version.to_s, paths: s.require_paths, files: s.files,
 platform: s.platform.to_s, dependencies: s.dependencies.map { |d| [d.name, d.type.to_s, d.requirement.to_s] })"""
    result = subprocess.check_output(
        ["ruby", "-rrubygems/package", "-rjson", "-e", command, str(gem), version],
        text=True,
    )
    spec = json.loads(result)
    if (
        spec["name"],
        spec["version"],
        spec["licenses"],
        spec["ruby"],
        spec["paths"],
        spec["platform"],
    ) != (
        PACKAGES["ruby"],
        spec["release_version"],
        ["Apache-2.0"],
        ">= 3.3",
        ["lib"],
        "ruby",
    ):
        raise ValueError(
            "Ruby package identity, license or runtime requirements differ"
        )
    if spec["dependencies"] != [["base64", "runtime", "~> 0.2"]] or set(
        spec["files"]
    ) != set(expected):
        raise ValueError("Ruby package files or Base64 runtime requirement differ")


def source_version(package, version):
    workspace = tomllib.loads(regular(ROOT / "Cargo.toml").decode())["workspace"][
        "package"
    ]["version"]
    if workspace != version:
        raise ValueError("release version differs from the source workspace")
    source = ROOT / "runtimes" / package
    if package == "java":
        check_pom(regular(source / "pom.xml"), version)
    elif package == "csharp":
        if (
            text(
                xml(regular(source / "Tungsten.Runtime.csproj")),
                "PropertyGroup/Version",
            )
            != version
        ):
            raise ValueError("C# source version differs from release")
    elif not re.search(
        r'VERSION\s*=\s*"' + re.escape(version) + '"',
        regular(source / "lib/tungsten/runtime/version.rb").decode(),
    ):
        raise ValueError("Ruby source version differs from release")


def compare_java_build(inventory, directory):
    for name, digest in inventory["files"].items():
        path = (
            ROOT / "runtimes/java/pom.xml"
            if name.endswith(".pom")
            else directory / name
        )
        if hashlib.sha256(regular(path)).hexdigest() != digest:
            raise ValueError("final Maven build differs from checked archive: " + name)


def verify(directory, package, version):
    release_version(version)
    source_version(package, version)
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("native runtime directory must be a regular directory")
    manifest = json.loads(
        regular(directory / "artifacts.json"), object_pairs_hook=unique_object
    )
    if (
        not isinstance(manifest, dict)
        or type(manifest.get("schema")) is not int
        or manifest.get("schema") != 1
        or manifest.get("package") != package
        or manifest.get("version") != version
    ):
        raise ValueError("native runtime inventory identity differs from release")
    commit = source_commit()
    if manifest.get("source_commit") != commit or manifest.get(
        "source_digest"
    ) != source_digest(package):
        raise ValueError("native runtime archives are not bound to the checked source")
    files = manifest.get("files")
    expected = {
        "java": {
            f"tungsten-runtime-{version}{s}"
            for s in (".jar", "-sources.jar", "-javadoc.jar", ".pom")
        },
        "csharp": {f"Tungsten.Runtime.{version}.nupkg"},
        "ruby": {f"tungsten-runtime-{version}.gem"},
    }[package]
    if (
        not isinstance(files, dict)
        or set(files) != expected
        or {p.name for p in directory.iterdir()} != expected | {"artifacts.json"}
    ):
        raise ValueError("unexpected native runtime archive inventory")
    for name, digest in files.items():
        if (
            not isinstance(digest, str)
            or not re.fullmatch(r"[0-9a-f]{64}", digest)
            or hashlib.sha256(regular(directory / name)).hexdigest() != digest
        ):
            raise ValueError("native runtime checksum mismatch: " + name)
    if package == "java":
        check_java(directory, version)
    elif package == "csharp":
        check_csharp(directory, version, commit)
    else:
        check_ruby(directory, version)
    return manifest


def build(directory, package, version):
    release_version(version)
    source_version(package, version)
    if directory.is_symlink() or directory.exists() and any(directory.iterdir()):
        raise ValueError("use an empty regular native runtime output directory")
    directory.mkdir(parents=True, exist_ok=True)
    source = ROOT / "runtimes" / package
    commit = source_commit()
    if package == "java":
        run(
            [
                "mvn",
                "-B",
                "-Prelease-artifacts",
                "-DskipTests",
                "-Dproject.build.outputTimestamp="
                + os.environ.get("SOURCE_DATE_EPOCH", "315532802"),
                "clean",
                "package",
            ],
            source,
        )
        for suffix in (".jar", "-sources.jar", "-javadoc.jar"):
            shutil.copy2(
                source / "target" / f"tungsten-runtime-{version}{suffix}", directory
            )
        shutil.copy2(source / "pom.xml", directory / f"tungsten-runtime-{version}.pom")
    elif package == "csharp":
        run(
            [
                "dotnet",
                "pack",
                source / "Tungsten.Runtime.csproj",
                "-c",
                "Release",
                "-p:RepositoryCommit=" + commit,
                "-p:ContinuousIntegrationBuild=true",
                "-o",
                directory,
            ],
            ROOT,
        )
    else:
        run(
            [
                "gem",
                "build",
                "tungsten-runtime.gemspec",
                "--output",
                directory / f"tungsten-runtime-{version}.gem",
            ],
            source,
        )
    files = {
        p.name: hashlib.sha256(regular(p)).hexdigest()
        for p in sorted(directory.iterdir())
    }
    (directory / "artifacts.json").write_text(
        json.dumps(
            {
                "schema": 1,
                "package": package,
                "version": version,
                "source_commit": commit,
                "source_digest": source_digest(package),
                "files": files,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    verify(directory, package, version)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", choices=PACKAGES, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--build", action="store_true")
    parser.add_argument(
        "--compare-build",
        type=Path,
        help="legacy read-only comparison of an existing Java build",
    )
    args = parser.parse_args()
    try:
        directory = args.directory.absolute()
        if args.build:
            if args.compare_build is not None:
                raise ValueError("build and comparison modes are mutually exclusive")
            build(directory, args.package, args.version)
        else:
            inventory = verify(directory, args.package, args.version)
            if args.compare_build is not None:
                if args.package != "java":
                    raise ValueError("build comparison is only used for Maven Central")
                compare_java_build(inventory, args.compare_build)
    except (
        OSError,
        ValueError,
        KeyError,
        TypeError,
        ET.ParseError,
        tarfile.TarError,
        zipfile.BadZipFile,
        subprocess.CalledProcessError,
    ) as error:
        parser.exit(1, f"native runtime artifacts: {error}\n")
    print(f"Validated {args.package} {args.version} native runtime archives")


if __name__ == "__main__":
    main()
