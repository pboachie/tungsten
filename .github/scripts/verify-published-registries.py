#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Compare anonymous public registry downloads with the tested release archives.

Never authenticates, uploads, installs or changes the supplied artifact directory.
NuGet may add a repository signature; signature trust is checked separately by
``dotnet nuget verify``, while this check preserves every original ZIP entry.
"""

from __future__ import annotations

import argparse
import base64
import email.parser
import hashlib
import http.client
import io
import json
import re
import stat
import sys
import tarfile
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

PACKAGES = ("npm", "python", "rust", "java", "kotlin", "csharp", "ruby")
KOTLIN_ARTIFACTS = ("tungsten-runtime-kotlin", "tungsten-runtime-kotlin-coroutines")
LIMIT = 128 * 1024 * 1024
HOSTS = {
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "repo.maven.apache.org",
    "rubygems.org",
    "api.nuget.org",
}
NPM = ("@elysra/tungsten-runtime", "@elysra/tungsten-mcp", "@elysra/tungsten-webmcp")


class PendingPublication(ValueError):
    """A public asset is not visible yet, without any conflicting bytes."""


class NetworkUnavailable(Exception):
    """A transient read failure, distinct from local filesystem failures."""


def checked_url(url, hosts=HOSTS):
    parts = urllib.parse.urlsplit(url)
    if (
        parts.scheme != "https"
        or parts.hostname not in hosts
        or parts.port not in (None, 443)
        or parts.username is not None
        or parts.password is not None
        or parts.fragment
    ):
        raise ValueError(
            "registry asset URL is not an anonymous canonical HTTPS endpoint"
        )
    return url


class RegistryRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        checked_url(url)
        return super().redirect_request(request, response, code, message, headers, url)


def download(url):
    """Return bounded response bytes without ambient proxy/auth/cookie handlers."""
    checked_url(url)
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), RegistryRedirect()
    )
    request = urllib.request.Request(
        url, headers={"User-Agent": "tungsten-release-verifier/1"}
    )
    with opener.open(request, timeout=30) as response:
        data = response.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise ValueError("registry response exceeds the size limit")
    return data


def fetch_data(fetch, url):
    checked_url(url)
    try:
        data = fetch(url)
    except urllib.error.HTTPError:
        raise
    except (urllib.error.URLError, OSError, http.client.HTTPException) as error:
        raise NetworkUnavailable("anonymous registry request failed") from error
    if not isinstance(data, bytes) or len(data) > LIMIT:
        raise ValueError("registry response must be bounded bytes")
    return data


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def json_object(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate registry JSON field")
            result[key] = value
        return result

    if len(data) > 2 * 1024 * 1024:
        raise ValueError("registry metadata exceeds the size limit")
    result = json.loads(data, object_pairs_hook=unique)
    if not isinstance(result, dict):
        raise TypeError("registry metadata must be a JSON object")
    return result


def regular(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > LIMIT:
        raise ValueError("expected a bounded regular archive: " + path.name)
    return path.read_bytes()


def safe_member(name):
    if not name or "\\" in name or ":" in name or name.startswith("/") or "\0" in name:
        raise ValueError("unsafe archive member")
    if any(part in ("", ".", "..") for part in name.rstrip("/").split("/")):
        raise ValueError("unsafe archive member")
    return name


def zip_entries(data):
    entries, size = {}, 0
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for member in archive.infolist():
            name = safe_member(member.filename)
            mode = stat.S_IFMT(member.external_attr >> 16)
            if (
                name in entries
                or mode not in (0, stat.S_IFREG, stat.S_IFDIR)
                or member.flag_bits & 1
            ):
                raise ValueError("duplicate, encrypted or non-regular ZIP entry")
            size += member.file_size
            if size > LIMIT:
                raise ValueError("expanded ZIP exceeds the size limit")
            entries[name] = archive.read(member)
    return entries


def tar_entries(data):
    entries, seen, size = {}, set(), 0
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive:
            name = safe_member(member.name)
            if name in seen or not (member.isfile() or member.isdir()):
                raise ValueError("duplicate or non-regular tar entry")
            seen.add(name)
            size += member.size
            if size > LIMIT:
                raise ValueError("expanded tar exceeds the size limit")
            if member.isfile():
                entries[name] = archive.extractfile(member).read()
    return entries


def xml_fields(data):
    if b"<!DOCTYPE" in data.upper() or b"<!ENTITY" in data.upper():
        raise ValueError("XML declarations are not permitted in package metadata")
    root = ET.fromstring(data)
    for node in root.iter():
        node.tag = node.tag.rsplit("}", 1)[-1]
    return root


def identity(actual_name, actual_version, name, version):
    if actual_name != name or actual_version != version:
        raise ValueError("package identity/version differs from the tested release")


def release_version(version):
    match = re.fullmatch(
        r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?",
        version,
    )
    if not match or any(
        part.isdigit() and len(part) > 1 and part.startswith("0")
        for part in (match[4] or "").split(".")
    ):
        raise ValueError("expected a strict release version")


def python_version(version):
    match = re.fullmatch(r"(\d+\.\d+\.\d+)(?:-(a|alpha|b|beta|rc)[.]?(\d+))?", version)
    if not match:
        raise ValueError(
            "Python publishing requires a PEP 440 compatible release version"
        )
    suffix = {"alpha": "a", "beta": "b"}.get(match[2], match[2])
    return match[1] + (suffix + match[3] if suffix else "")


def ruby_version(version):
    # Gem::Version normalizes each SemVer hyphen, including inside identifiers.
    return version.replace("-", ".pre.")


def nuget_metadata(data, version):
    root = xml_fields(data)
    identity(
        root.findtext("metadata/id"),
        root.findtext("metadata/version"),
        "Tungsten.Runtime",
        version,
    )


def python_metadata(data, version):
    headers = email.parser.BytesParser().parsebytes(data)
    names, versions = headers.get_all("Name", []), headers.get_all("Version", [])
    if len(names) != 1 or len(versions) != 1:
        raise ValueError("Python archive must have one package name and version")
    name = re.sub(r"[-_.]+", "-", names[0]).lower()
    identity(name, versions[0], "tungsten-runtime", python_version(version))


def local_archives(package, version, directory):
    if package not in PACKAGES:
        raise ValueError("unknown registry package")
    release_version(version)
    directory = Path(directory)
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("expected an artifact directory")
    if package == "npm":
        names = {
            name.removeprefix("@").replace("/", "-") + f"-{version}.tgz" for name in NPM
        }
        actual = {path.name for path in directory.glob("*.tgz")}
    elif package == "python":
        normalized = python_version(version)
        wheels, sdists = list(directory.glob("*.whl")), list(directory.glob("*.tar.gz"))
        if len(wheels) != 1 or len(sdists) != 1:
            raise ValueError("expected one tested Python wheel and sdist")
        names = {wheels[0].name, sdists[0].name}
        actual = names
        if names != {
            f"tungsten_runtime-{normalized}-py3-none-any.whl",
            f"tungsten_runtime-{normalized}.tar.gz",
        }:
            raise ValueError("Python archive filenames differ from release")
    else:
        names = {
            "rust": {
                f"{name}-{version}.crate"
                for name in ("tungsten-runtime", "tungsten-cli-kit")
            },
            "java": {
                f"tungsten-runtime-{version}{suffix}"
                for suffix in (".jar", "-sources.jar", "-javadoc.jar", ".pom")
            },
            "kotlin": {
                f"{artifact}-{version}{suffix}"
                for artifact in KOTLIN_ARTIFACTS
                for suffix in (".jar", "-sources.jar", "-javadoc.jar", ".pom")
            },
            "csharp": {f"Tungsten.Runtime.{version}.nupkg"},
            "ruby": {f"tungsten-runtime-{version}.gem"},
        }[package]
        patterns = {
            "rust": ("*.crate",),
            "java": ("*.jar", "*.pom"),
            "kotlin": ("*.jar", "*.pom"),
            "csharp": ("*.nupkg",),
            "ruby": ("*.gem",),
        }[package]
        actual = {path.name for pattern in patterns for path in directory.glob(pattern)}
    if actual != names:
        raise ValueError("missing or unexpected tested registry archives")
    files = {name: regular(directory / name) for name in sorted(names)}
    inventory = directory / "artifacts.json"
    if inventory.exists() or inventory.is_symlink():
        manifest = json_object(regular(inventory))
        if manifest.get("package") != package or manifest.get("version") != version:
            raise ValueError("artifact inventory identity differs from release")
        if manifest.get("files") != {
            name: sha256(data) for name, data in files.items()
        }:
            raise ValueError("artifact inventory checksums differ from tested archives")
    checksums = directory / "SHA256SUMS"
    if checksums.exists() or checksums.is_symlink():
        listed = {}
        for line in regular(checksums).decode().splitlines():
            match = re.fullmatch(r"([0-9a-f]{64}) [ *]([^/\\]+)", line)
            if not match or match[2] in listed:
                raise ValueError("invalid or duplicate tested archive checksum")
            listed[match[2]] = match[1]
        if listed != {name: sha256(data) for name, data in files.items()}:
            raise ValueError("tested archive checksum list differs from archives")
    if package == "npm":
        for name in NPM:
            filename = name.removeprefix("@").replace("/", "-") + f"-{version}.tgz"
            metadata = json_object(tar_entries(files[filename])["package/package.json"])
            identity(metadata.get("name"), metadata.get("version"), name, version)
    elif package == "python":
        for name, data in files.items():
            entries = zip_entries(data) if name.endswith(".whl") else tar_entries(data)
            metadata = [
                value
                for key, value in entries.items()
                if key.endswith((".dist-info/METADATA", "/PKG-INFO"))
            ]
            if len(metadata) != 1:
                raise ValueError("expected one Python package metadata document")
            python_metadata(metadata[0], version)
    elif package == "rust":
        for name, data in files.items():
            crate = name.removesuffix(f"-{version}.crate")
            metadata = tomllib.loads(
                tar_entries(data)[f"{crate}-{version}/Cargo.toml"].decode()
            )["package"]
            identity(metadata.get("name"), metadata.get("version"), crate, version)
    elif package == "java":
        metadata = xml_fields(files[f"tungsten-runtime-{version}.pom"])
        identity(
            metadata.findtext("artifactId"),
            metadata.findtext("version"),
            "tungsten-runtime",
            version,
        )
        if metadata.findtext("groupId") != "io.github.pboachie.tungsten":
            raise ValueError("unexpected Maven group")
    elif package == "kotlin":
        for artifact in KOTLIN_ARTIFACTS:
            metadata = xml_fields(files[f"{artifact}-{version}.pom"])
            identity(
                metadata.findtext("artifactId"),
                metadata.findtext("version"),
                artifact,
                version,
            )
            if metadata.findtext("groupId") != "io.github.pboachie.tungsten":
                raise ValueError("unexpected Maven group")
    elif package == "csharp":
        nuget_metadata(
            zip_entries(next(iter(files.values())))["Tungsten.Runtime.nuspec"], version
        )
    return files


def exact_file(name, original, url, fetch):
    checked_url(url)
    published = fetch_data(fetch, url)
    if published != original:
        raise ValueError("published archive differs from the tested bytes: " + name)
    return {
        "name": name,
        "sha256": sha256(original),
        "published_sha256": sha256(published),
        "url": url,
    }


def save_public_package(directory, name, data):
    directory = Path(directory)
    if directory.is_symlink() or directory.exists() and not directory.is_dir():
        raise ValueError("verified download destination must be a regular directory")
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / name
    if path.exists() or path.is_symlink():
        if regular(path) != data:
            raise ValueError("refusing to overwrite a different verified download")
    else:
        with path.open("xb") as destination:
            destination.write(data)
    return str(path.resolve())


def verify_files(package, version, files, fetch, save_download=None):
    results = []
    if package == "npm":
        for name in NPM:
            filename = name.removeprefix("@").replace("/", "-") + f"-{version}.tgz"
            data = files[filename]
            url = (
                "https://registry.npmjs.org/"
                + urllib.parse.quote(name, safe="")
                + "/"
                + version
            )
            metadata = json_object(fetch_data(fetch, url))
            identity(metadata.get("name"), metadata.get("version"), name, version)
            dist = metadata["dist"]
            if dist.get("shasum") != hashlib.sha1(data).hexdigest():
                raise ValueError("npm registry checksum differs from tested archive")
            if "integrity" in dist:
                expected = (
                    "sha512-" + base64.b64encode(hashlib.sha512(data).digest()).decode()
                )
                if dist["integrity"] != expected:
                    raise ValueError(
                        "npm registry integrity differs from tested archive"
                    )
            results.append(
                exact_file(
                    filename,
                    data,
                    checked_url(dist["tarball"], {"registry.npmjs.org"}),
                    fetch,
                )
            )
    elif package == "python":
        normalized = python_version(version)
        metadata = json_object(
            fetch_data(
                fetch, f"https://pypi.org/pypi/tungsten-runtime/{normalized}/json"
            )
        )
        identity(
            re.sub(r"[-_.]+", "-", metadata["info"]["name"]).lower(),
            metadata["info"]["version"],
            "tungsten-runtime",
            normalized,
        )
        assets = {}
        for item in metadata["urls"]:
            if item["filename"] in assets:
                raise ValueError("duplicate PyPI release asset")
            assets[item["filename"]] = item
        if set(assets) - set(files):
            raise ValueError("unexpected PyPI release asset")
        if set(files) - set(assets):
            raise PendingPublication("the complete PyPI release is not public yet")
        for name, data in files.items():
            item = assets[name]
            if item["digests"]["sha256"] != sha256(data):
                raise ValueError("PyPI checksum differs from tested archive")
            results.append(
                exact_file(
                    name,
                    data,
                    checked_url(item["url"], {"files.pythonhosted.org"}),
                    fetch,
                )
            )
    elif package == "rust":
        for name, data in files.items():
            crate = name.removesuffix(f"-{version}.crate")
            base = f"https://crates.io/api/v1/crates/{crate}/{version}"
            metadata = json_object(fetch_data(fetch, base))["version"]
            identity(metadata.get("crate"), metadata.get("num"), crate, version)
            if metadata.get("checksum") != sha256(data):
                raise ValueError("crates.io checksum differs from tested archive")
            results.append(exact_file(name, data, base + "/download", fetch))
    elif package == "java":
        base = f"https://repo.maven.apache.org/maven2/io/github/pboachie/tungsten/tungsten-runtime/{version}/"
        for name, data in files.items():
            results.append(exact_file(name, data, base + name, fetch))
    elif package == "kotlin":
        for name, data in files.items():
            artifact = next(a for a in KOTLIN_ARTIFACTS if name.startswith(f"{a}-{version}"))
            base = f"https://repo.maven.apache.org/maven2/io/github/pboachie/tungsten/{artifact}/{version}/"
            results.append(exact_file(name, data, base + name, fetch))
    elif package == "ruby":
        normalized = ruby_version(version)
        metadata = json_object(
            fetch_data(
                fetch,
                f"https://rubygems.org/api/v2/rubygems/tungsten-runtime/versions/{normalized}.json",
            )
        )
        identity(
            metadata.get("name"),
            metadata.get("version"),
            "tungsten-runtime",
            normalized,
        )
        name, data = next(iter(files.items()))
        if metadata.get("sha") != sha256(data):
            raise ValueError("RubyGems checksum differs from tested archive")
        url = f"https://rubygems.org/downloads/tungsten-runtime-{normalized}.gem"
        results.append(exact_file(name, data, url, fetch))
    else:
        base = f"https://api.nuget.org/v3-flatcontainer/tungsten.runtime/{version.lower()}/"
        nuget_metadata(fetch_data(fetch, base + "tungsten.runtime.nuspec"), version)
        name, original = next(iter(files.items()))
        url = base + f"tungsten.runtime.{version.lower()}.nupkg"
        published = fetch_data(fetch, url)
        tested, public = zip_entries(original), zip_entries(published)
        if set(public) - set(tested) - {".signature.p7s"} or any(
            public.get(key) != value for key, value in tested.items()
        ):
            raise ValueError(
                "NuGet changed a tested ZIP entry or added an unexpected entry"
            )
        nuget_metadata(public["Tungsten.Runtime.nuspec"], version)
        result = {
            "name": name,
            "sha256": sha256(original),
            "published_sha256": sha256(published),
            "url": url,
            "original_entries_preserved": len(tested),
            "added_repository_signature": ".signature.p7s" not in tested
            and ".signature.p7s" in public,
        }
        if save_download is not None:
            result["download_path"] = save_public_package(
                save_download, name, published
            )
        results.append(result)
    return {
        "schema": 1,
        "package": package,
        "version": version,
        "verified": True,
        "anonymous": True,
        "artifacts": results,
    }


def check_save_directory(package, directory, save_download):
    if save_download is not None:
        target, original = Path(save_download).resolve(), Path(directory).resolve()
        if package != "csharp" or target == original or original in target.parents:
            raise ValueError(
                "saved public downloads require a separate NuGet-only directory"
            )


def verify_once(package, version, directory, fetch=download, save_download=None):
    check_save_directory(package, directory, save_download)
    return verify_files(
        package,
        version,
        local_archives(package, version, directory),
        fetch,
        save_download,
    )


def verify(
    package,
    version,
    directory,
    attempts=30,
    delay=30,
    fetch=download,
    sleep=time.sleep,
    save_download=None,
):
    if (
        isinstance(attempts, bool)
        or not isinstance(attempts, int)
        or not 1 <= attempts <= 120
        or not 0 <= delay <= 60
    ):
        raise ValueError("attempts must be 1..120 and delay must be 0..60 seconds")
    check_save_directory(package, directory, save_download)
    files = local_archives(package, version, directory)
    for attempt in range(attempts):
        try:
            return verify_files(package, version, files, fetch, save_download)
        except urllib.error.HTTPError as error:
            error.close()
            if error.code not in (404, 408, 429) and not 500 <= error.code <= 599:
                raise
            failure = error
        except (PendingPublication, NetworkUnavailable) as error:
            failure = error
        if attempt + 1 == attempts:
            raise ValueError(
                "public registry verification did not converge within the retry limit"
            ) from failure
        print(
            f"Public {package} assets pending; retry {attempt + 2}/{attempts} in {delay}s",
            file=sys.stderr,
        )
        sleep(delay)
    raise AssertionError("unreachable retry state")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--package", choices=PACKAGES, required=True)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--attempts", type=int, default=30)
    parser.add_argument("--delay", type=float, default=30)
    parser.add_argument(
        "--save-download",
        type=Path,
        help="save only the verified public NuGet package for a separate signature check",
    )
    args = parser.parse_args(argv)
    try:
        result = verify(
            args.package,
            args.version,
            args.directory,
            args.attempts,
            args.delay,
            save_download=args.save_download,
        )
    except (
        OSError,
        ValueError,
        KeyError,
        TypeError,
        ET.ParseError,
        tarfile.TarError,
        zipfile.BadZipFile,
    ) as error:
        parser.exit(1, f"published registry verification: {error}\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
