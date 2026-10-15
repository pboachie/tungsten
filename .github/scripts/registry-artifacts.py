#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Stage or verify source-bound Python and Rust packages; never publishes."""

from __future__ import annotations

import argparse
import base64
import csv
import email.parser
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import tarfile
import tomllib
import zipfile
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "native_artifacts", Path(__file__).with_name("native-runtime-artifacts.py")
)
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)
ROOT = native.ROOT


def source_files(package):
    base = ROOT / "runtimes" / package
    patterns = (
        ["src/**/*.py", "src/**/py.typed"]
        if package == "python"
        else ["runtime/src/**/*.rs", "cli-kit/src/**/*.rs"]
    )
    files = [path for pattern in patterns for path in base.glob(pattern)]
    names = (
        ["pyproject.toml", "README.md", "LICENSE"]
        if package == "python"
        else [
            "Cargo.toml",
            "Cargo.lock",
            "runtime/Cargo.toml",
            "runtime/README.md",
            "runtime/LICENSE",
            "cli-kit/Cargo.toml",
            "cli-kit/README.md",
            "cli-kit/LICENSE",
        ]
    )
    files += [base / name for name in names]
    return {path.relative_to(ROOT).as_posix(): native.regular(path) for path in files}


def source_digest(package):
    digest = hashlib.sha256()
    for name, content in sorted(source_files(package).items()):
        digest.update(name.encode() + b"\0" + hashlib.sha256(content).digest())
    return digest.hexdigest()


def source_version(package, version):
    native.release_version(version)
    workspace = tomllib.loads(native.regular(ROOT / "Cargo.toml").decode())[
        "workspace"
    ]["package"]["version"]
    manifest = tomllib.loads(
        native.regular(
            ROOT
            / "runtimes"
            / package
            / ("pyproject.toml" if package == "python" else "Cargo.toml")
        ).decode()
    )
    if workspace != version:
        raise ValueError("release version differs from source workspace")
    if package == "python":
        if (
            manifest["project"]["name"] != "tungsten-runtime"
            or manifest["project"]["version"] != version
        ):
            raise ValueError("Python source identity differs from release")
    elif (
        manifest["workspace"]["package"]["version"] != version
        or manifest["workspace"]["package"]["publish"] != ["crates-io"]
        or manifest["workspace"]["dependencies"]["tungsten-runtime"]["version"]
        != version
    ):
        raise ValueError("Rust workspace identity or dependency differs from release")


def expected_names(package, version):
    if package == "rust":
        return {
            f"{name}-{version}.crate"
            for name in ("tungsten-runtime", "tungsten-cli-kit")
        }
    # PEP 440 normalizes supported SemVer prereleases (e.g. 0.1.3-rc.1).
    match = re.fullmatch(r"(\d+\.\d+\.\d+)(?:-(a|alpha|b|beta|rc)[.]?(\d+))?", version)
    if not match:
        raise ValueError(
            "Python publishing requires a PEP 440 compatible release version"
        )
    normalized = match[1] + (
        {"alpha": "a", "beta": "b"}.get(match[2], match[2]) + match[3]
        if match[2]
        else ""
    )
    return {
        f"tungsten_runtime-{normalized}-py3-none-any.whl",
        f"tungsten_runtime-{normalized}.tar.gz",
    }


def check_metadata(data, version):
    metadata = email.parser.BytesParser().parsebytes(data)
    normalized = (
        next(iter(expected_names("python", version)))
        .split("-")[1]
        .removesuffix(".tar.gz")
    )
    if metadata.get_all("Name") != ["tungsten-runtime"] or metadata.get_all(
        "Version"
    ) != [normalized]:
        raise ValueError("Python archive identity differs from release")
    if (
        metadata.get_all("Requires-Python") != [">=3.10"]
        or metadata.get_all("License-Expression") != ["Apache-2.0"]
        or metadata.get_all("License-File") != ["LICENSE"]
        or metadata.get_all("Description-Content-Type") != ["text/markdown"]
    ):
        raise ValueError(
            "Python archive runtime requirements, license or description type differ"
        )
    if metadata.get_payload(decode=True) != native.regular(
        ROOT / "runtimes/python/README.md"
    ):
        raise ValueError("Python archive description differs from the source README")
    project = tomllib.loads(
        native.regular(ROOT / "runtimes/python/pyproject.toml").decode()
    )["project"]
    if sorted(
        requirement(value) for value in metadata.get_all("Requires-Dist", [])
    ) != sorted(requirement(value) for value in project.get("dependencies", [])):
        raise ValueError("Python archive dependencies differ from the source manifest")


def requirement(value):
    # The runtime uses named registry requirements with simple version ranges
    # and one Python-version marker. Refuse new forms until explicitly reviewed.
    match = re.fullmatch(
        r"([A-Za-z0-9][A-Za-z0-9_.-]*)([^;]*)(?:;\s*(.*))?", value.strip()
    )
    if not match:
        raise ValueError("unsupported Python runtime dependency")
    constraints = [part.strip() for part in match[2].split(",")]
    if any(
        not re.fullmatch(r"(?:~=|==|!=|<=|>=|<|>)[0-9][0-9A-Za-z.*+!-]*", part)
        for part in constraints
    ):
        raise ValueError(
            "expected a registry version range without direct URLs or extras"
        )
    marker = (match[3] or "").strip().replace('"', "'")
    if marker and not re.fullmatch(
        r"python_version\s*(?:<|<=|==|>=|>)\s*'[0-9]+\.[0-9]+'", marker
    ):
        raise ValueError("unsupported Python dependency marker")
    return (
        re.sub(r"[-_.]+", "-", match[1]).lower(),
        tuple(sorted(constraints)),
        re.sub(r"\s+", "", marker),
    )


def check_record(files, record):
    rows = list(csv.reader(io.StringIO(files[record].decode("utf-8"))))
    if (
        any(len(row) != 3 for row in rows)
        or len({row[0] for row in rows}) != len(rows)
        or {row[0] for row in rows} != set(files)
    ):
        raise ValueError("wheel RECORD does not describe every member exactly once")
    for name, digest, size in rows:
        if name == record:
            if digest or size:
                raise ValueError("wheel RECORD must not hash itself")
            continue
        expected = (
            base64.urlsafe_b64encode(hashlib.sha256(files[name]).digest())
            .rstrip(b"=")
            .decode()
        )
        if digest != "sha256=" + expected or size != str(len(files[name])):
            raise ValueError("wheel RECORD content digest differs")


def check_python(directory, version):
    base = ROOT / "runtimes/python"
    for name in expected_names("python", version):
        if name.endswith(".whl"):
            files = native.zip_files(directory / name)
            metadata = [key for key in files if key.endswith(".dist-info/METADATA")]
            if len(metadata) != 1:
                raise ValueError("expected exactly one wheel metadata file")
            check_metadata(files[metadata[0]], version)
            dist_info = metadata[0].removesuffix("METADATA")
            if dist_info != name.removesuffix("-py3-none-any.whl") + ".dist-info/":
                raise ValueError("wheel metadata directory differs from its identity")
            allowed = {
                dist_info + suffix
                for suffix in ("METADATA", "WHEEL", "RECORD", "licenses/LICENSE")
            }
            if {
                key for key in files if not key.startswith("tungsten_runtime/")
            } != allowed:
                raise ValueError(
                    "wheel has unexpected files outside the runtime payload"
                )
            if files[dist_info + "licenses/LICENSE"] != native.regular(
                base / "LICENSE"
            ):
                raise ValueError("wheel license differs from source")
            wheel = email.parser.BytesParser().parsebytes(files[dist_info + "WHEEL"])
            if wheel.get_all("Root-Is-Purelib") != ["true"] or wheel.get_all("Tag") != [
                "py3-none-any"
            ]:
                raise ValueError(
                    "wheel does not have the declared portable Python target"
                )
            check_record(files, dist_info + "RECORD")
            prefix = ""
        else:
            entries = native.tar_files(native.regular(directory / name), "r:gz")
            roots = {key.split("/")[0] for key in entries}
            if len(roots) != 1:
                raise ValueError("sdist has multiple roots")
            root = next(iter(roots)) + "/"
            files = {key.removeprefix(root): data for key, data in entries.items()}
            check_metadata(files["PKG-INFO"], version)
            if files.get("pyproject.toml") != native.regular(base / "pyproject.toml"):
                raise ValueError("sdist manifest differs from source")
            allowed = {"PKG-INFO", "pyproject.toml", "README.md", "LICENSE"}
            for extra in ("uv.lock", ".gitignore"):
                if (base / extra).is_file():
                    allowed.add(extra)
                    if files.get(extra) != native.regular(base / extra):
                        raise ValueError("sdist auxiliary file differs from source")
            if {
                key for key in files if not key.startswith("src/tungsten_runtime/")
            } != allowed:
                raise ValueError(
                    "sdist has unexpected files outside the runtime payload"
                )
            for notice in ("README.md", "LICENSE"):
                if files.get(notice) != native.regular(base / notice):
                    raise ValueError("sdist notices differ from source")
            prefix = "src/"
        expected = {
            p.relative_to(base / "src").as_posix(): native.regular(p)
            for p in (base / "src").rglob("*")
            if p.is_file() and (p.suffix == ".py" or p.name == "py.typed")
        }
        actual = {
            key.removeprefix(prefix): data
            for key, data in files.items()
            if key.startswith(prefix + "tungsten_runtime/")
        }
        if actual != expected:
            raise ValueError("Python package payload differs from source")


def dependency_semantics(name, value):
    if isinstance(value, str):
        value = {"version": value}
    if (
        not isinstance(value, dict)
        or not set(value)
        <= {"version", "features", "default-features", "optional", "package"}
        or not isinstance(value.get("version"), str)
    ):
        raise ValueError("crate has a non-registry or unsupported dependency")
    if any(
        type(value.get(key, default)) is not bool
        for key, default in (("optional", False), ("default-features", True))
    ):
        raise ValueError("crate dependency flags must be booleans")
    features = value.get("features", [])
    if (
        not isinstance(features, list)
        or any(not isinstance(feature, str) for feature in features)
        or len(set(features)) != len(features)
    ):
        raise ValueError("crate dependency features must be unique strings")
    return {
        "version": value["version"],
        "features": sorted(value.get("features", [])),
        "default-features": value.get("default-features", True),
        "optional": value.get("optional", False),
        "package": value.get("package", name),
    }


def dependency_tables(manifest, workspace=None):
    tables = {None: manifest, **manifest.get("target", {})}
    result = {}
    for target, table in tables.items():
        for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
            for name, value in table.get(kind, {}).items():
                if isinstance(value, dict) and value.get("workspace") is True:
                    if workspace is None or set(value) != {"workspace"}:
                        raise ValueError("unsupported inherited dependency override")
                    value = workspace["dependencies"][name]
                    value = (
                        {"version": value} if isinstance(value, str) else dict(value)
                    )
                    if "path" in value and (
                        name != "tungsten-runtime" or value.pop("path") != "runtime"
                    ):
                        raise ValueError("unexpected inherited path dependency")
                result[target, kind, name] = dependency_semantics(name, value)
    return result


def check_rust(directory, version):
    workspace = tomllib.loads(
        native.regular(ROOT / "runtimes/rust/Cargo.toml").decode()
    )["workspace"]
    for name, member in (
        ("tungsten-runtime", "runtime"),
        ("tungsten-cli-kit", "cli-kit"),
    ):
        archive = native.tar_files(
            native.regular(directory / f"{name}-{version}.crate"), "r:gz"
        )
        prefix = f"{name}-{version}/"
        if any(not key.startswith(prefix) for key in archive):
            raise ValueError("crate archive has unexpected root")
        files = {key.removeprefix(prefix): data for key, data in archive.items()}
        manifest = tomllib.loads(files["Cargo.toml"].decode())
        package = manifest["package"]
        if (
            package["name"] != name
            or package["version"] != version
            or package["license"] != "Apache-2.0"
            or package["publish"] != ["crates-io"]
            or package.get("rust-version") != "1.88"
            or package.get("edition") != "2024"
            or package.get("build") is not False
        ):
            raise ValueError("crate manifest identity differs from release")
        if any(
            key in manifest
            for key in ("patch", "replace", "bin", "example", "test", "bench")
        ):
            raise ValueError(
                "crate has an unexpected build target or dependency override"
            )
        library = manifest.get("lib", {})
        if (
            library.get("path") != "src/lib.rs"
            or library.get("doctest") is not False
            or library.get("proc-macro", False) is not False
            or library.get("crate-type", ["lib"]) != ["lib"]
        ):
            raise ValueError("crate has an unexpected library target")
        if (
            member == "cli-kit"
            and manifest["dependencies"]["tungsten-runtime"]["version"] != version
        ):
            raise ValueError("CLI-kit runtime dependency differs from release")
        base = ROOT / "runtimes/rust" / member
        original = tomllib.loads(native.regular(base / "Cargo.toml").decode())
        if dependency_tables(manifest) != dependency_tables(original, workspace):
            raise ValueError(
                "normalized crate dependencies differ from source workspace"
            )
        expected = {
            p.relative_to(base).as_posix(): native.regular(p)
            for p in (base / "src").rglob("*.rs")
        }
        if {
            key: data for key, data in files.items() if key.startswith("src/")
        } != expected:
            raise ValueError("crate source payload differs from checkout")
        for archived, original in (
            ("Cargo.toml.orig", "Cargo.toml"),
            ("README.md", "README.md"),
            ("LICENSE", "LICENSE"),
        ):
            if files.get(archived) != native.regular(base / original):
                raise ValueError("crate manifest or notices differ from source")
        auxiliary = {
            "Cargo.toml",
            "Cargo.toml.orig",
            "Cargo.lock",
            "README.md",
            "LICENSE",
            ".cargo_vcs_info.json",
        }
        if any(key not in auxiliary and not key.startswith("src/") for key in files):
            raise ValueError("crate has an unexpected auxiliary file or build script")
        vcs_data = files.get(".cargo_vcs_info.json")
        if vcs_data is not None:
            vcs = json.loads(vcs_data, object_pairs_hook=native.unique_object)
            if (
                vcs.get("git", {}).get("sha1") != native.source_commit()
                or vcs.get("path_in_vcs") != "runtimes/rust/" + member
            ):
                raise ValueError("crate VCS metadata differs from source checkout")
            if (
                os.environ.get("CI")
                and vcs.get("git", {}).get("dirty", False) is not False
            ):
                raise ValueError("CI crate must come from a clean source checkout")
        elif os.environ.get("CI"):
            raise ValueError("CI crate must include source VCS metadata")


def verify(directory, package, version):
    source_version(package, version)
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("artifact directory must be regular")
    inventory = json.loads(
        native.regular(directory / "artifacts.json"),
        object_pairs_hook=native.unique_object,
    )
    if (
        inventory.get("schema"),
        inventory.get("package"),
        inventory.get("version"),
    ) != (1, package, version):
        raise ValueError("artifact inventory identity differs from release")
    if inventory.get("source_commit") != native.source_commit() or inventory.get(
        "source_digest"
    ) != source_digest(package):
        raise ValueError("archives are not bound to the checked source")
    names = expected_names(package, version)
    if (
        not isinstance(inventory.get("files"), dict)
        or set(inventory["files"]) != names
        or {p.name for p in directory.iterdir()} != names | {"artifacts.json"}
    ):
        raise ValueError("unexpected artifact inventory")
    for name, digest in inventory["files"].items():
        if (
            not isinstance(digest, str)
            or not re.fullmatch(r"[0-9a-f]{64}", digest)
            or hashlib.sha256(native.regular(directory / name)).hexdigest() != digest
        ):
            raise ValueError("artifact checksum mismatch")
    (check_python if package == "python" else check_rust)(directory, version)
    return inventory


def compare_build(inventory, directory):
    for name, digest in inventory["files"].items():
        if hashlib.sha256(native.regular(directory / name)).hexdigest() != digest:
            raise ValueError("fresh pre-publication build differs from tested artifact")


def stage(directory, package, version, stage_from):
    source_version(package, version)
    if directory.is_symlink() or directory.exists() and any(directory.iterdir()):
        raise ValueError("use an empty regular output directory")
    directory.mkdir(parents=True, exist_ok=True)
    for name in expected_names(package, version):
        native.regular(stage_from / name)
        shutil.copy2(stage_from / name, directory / name)
    inventory = {
        "schema": 1,
        "package": package,
        "version": version,
        "source_commit": native.source_commit(),
        "source_digest": source_digest(package),
        "files": {
            name: hashlib.sha256(native.regular(directory / name)).hexdigest()
            for name in sorted(expected_names(package, version))
        },
    }
    (directory / "artifacts.json").write_text(
        json.dumps(inventory, indent=2) + "\n", encoding="utf-8"
    )
    verify(directory, package, version)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", choices=["python", "rust"], required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--stage-from", type=Path)
    parser.add_argument("--compare-build", type=Path)
    args = parser.parse_args()
    try:
        if args.stage_from:
            stage(args.directory, args.package, args.version, args.stage_from)
        inventory = verify(args.directory, args.package, args.version)
        if args.compare_build:
            compare_build(inventory, args.compare_build)
    except (
        OSError,
        ValueError,
        KeyError,
        TypeError,
        tarfile.TarError,
        zipfile.BadZipFile,
    ) as error:
        parser.exit(1, f"registry artifacts: {error}\n")
    print(f"Validated {args.package} {args.version} source-bound archives")


if __name__ == "__main__":
    main()
