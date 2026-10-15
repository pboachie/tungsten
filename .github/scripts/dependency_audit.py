#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Check locked package sources, dependency growth and OSV advisories without installing code.

Python 3.11+, standard library only. --offline skips only the advisory query.
The allowlist is a review baseline, not an assertion that packages are safe.
Reviewed advisories are recorded as scoped, expiring exceptions in
.github/dependency-policy.json: one advisory id on one locked package, with a
reason and an expiry date. Every exception is printed in the job log; an
entry past its expiry fails the check.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
import urllib.request
from datetime import date
from pathlib import Path
from urllib.parse import urlsplit

LOCK_NAMES = {"Cargo.lock", "uv.lock", "package-lock.json"}
PACKAGING_LOCK = ".github/packaging-requirements.lock"
EXCEPTIONS_KEY = "advisory_exceptions"


def require(condition: object, message: str) -> None:
    if not condition:
        raise ValueError(message)


def workspace_members(directory: Path) -> set[str]:
    """Package names of the cargo workspace rooted at `directory` (root package included)."""
    manifest = directory / "Cargo.toml"
    require(manifest.is_file() and not manifest.is_symlink(), f"{manifest}: missing workspace manifest")
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    names = set()
    if "package" in data:
        names.add(data["package"]["name"])
    workspace = data.get("workspace", {})
    excluded = {(directory / pattern).resolve() for pattern in workspace.get("exclude", [])}
    for pattern in workspace.get("members", []):
        for member in sorted(directory.glob(pattern)):
            member_manifest = member / "Cargo.toml"
            if member.resolve() in excluded or not member_manifest.is_file():
                continue
            package = tomllib.loads(member_manifest.read_text(encoding="utf-8")).get("package", {})
            if "name" in package:
                names.add(package["name"])
    return names


def check_manifests(root: Path, files: list[str]) -> None:
    """Refuse source overrides: a [patch] or [replace] table swaps a locked dependency's code."""
    for file in files:
        name = Path(file).name
        if name != "Cargo.toml" and not (name in {"config.toml", "config"} and Path(file).parent.name == ".cargo"):
            continue
        path = root / file
        require(not path.is_symlink(), f"{file}: symbolic manifest")
        data = tomllib.loads(path.read_text(encoding="utf-8"))
        for table in ("patch", "replace"):
            require(table not in data, f"{file}: [{table}] source overrides are not allowed")


def inventory(root: Path, files: list[str]) -> dict[str, list[tuple[str, str, str]]]:
    result = {}
    for file in files:
        if Path(file).name not in LOCK_NAMES and file != PACKAGING_LOCK:
            continue
        path = root / file
        require(not path.is_symlink(), f"{file}: symbolic lockfile")
        entries = []
        if file == PACKAGING_LOCK:
            index_seen = False
            names = set()
            for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                line = raw.strip()
                if not line or line.startswith("#"):
                    continue
                if line == "--index-url https://pypi.org/simple":
                    require(not index_seen, f"{file}:{number}: duplicate index")
                    index_seen = True
                    continue
                match = re.fullmatch(r"([a-z0-9]+(?:-[a-z0-9]+)*)==([0-9][A-Za-z0-9.!+_-]*) --hash=sha256:([a-f0-9]{64})", line)
                require(match, f"{file}:{number}: expected one exact package pin and SHA-256 hash")
                name, version, _ = match.groups()
                require(name not in names, f"{file}:{number}: duplicate package {name}")
                names.add(name)
                entries.append(("PyPI", name, version))
            require(index_seen and entries, f"{file}: missing approved index or packages")
        elif path.name == "package-lock.json":
            lock = json.loads(path.read_text(encoding="utf-8"))
            require(lock.get("lockfileVersion") == 3, f"{file}: expected lockfile version 3")
            for location, package in lock["packages"].items():
                if "node_modules/" not in location:
                    continue
                if package.get("link"):
                    require(location == "node_modules/@elysra/tungsten-runtime"
                            and package.get("resolved") == "../ts"
                            and file in {"runtimes/mcp/package-lock.json", "runtimes/webmcp/package-lock.json"},
                            f"{file}: unreviewed local link {location}")
                    continue
                source = urlsplit(package.get("resolved", ""))
                require(source.scheme == "https" and source.netloc == "registry.npmjs.org",
                        f"{file}: unapproved package source for {location}")
                require(re.fullmatch(r"sha512-[A-Za-z0-9+/]{86}==", package.get("integrity", "")),
                        f"{file}: missing SHA-512 integrity for {location}")
                require(not package.get("hasInstallScript"), f"{file}: install hook for {location}")
                name = package.get("name", location.rsplit("node_modules/", 1)[1])
                entries.append(("npm", name, package["version"]))
        else:
            lock = tomllib.loads(path.read_text(encoding="utf-8"))
            members = workspace_members(path.parent) if path.name == "Cargo.lock" else set()
            for package in lock["package"]:
                source = package.get("source")
                if path.name == "Cargo.lock":
                    if source is None:
                        require(package["name"] in members,
                                f"{file}: source-less package {package['name']} is not a workspace member")
                        continue
                    require(source == "registry+https://github.com/rust-lang/crates.io-index",
                            f"{file}: unapproved source for {package['name']}")
                    require(re.fullmatch(r"[a-f0-9]{64}", package.get("checksum", "")),
                            f"{file}: missing checksum for {package['name']}")
                    ecosystem = "crates.io"
                else:
                    if source == {"editable": "."}:
                        require(package["name"] == "tungsten-runtime", f"{file}: unexpected editable package")
                        continue
                    require(source == {"registry": "https://pypi.org/simple"},
                            f"{file}: unapproved source for {package['name']}")
                    artifacts = package.get("wheels", []) + ([package["sdist"]] if "sdist" in package else [])
                    require(artifacts, f"{file}: no hashed artifacts for {package['name']}")
                    for artifact in artifacts:
                        url = urlsplit(artifact["url"])
                        require(url.scheme == "https" and url.netloc == "files.pythonhosted.org"
                                and re.fullmatch(r"sha256:[a-f0-9]{64}", artifact.get("hash", "")),
                                f"{file}: unapproved artifact for {package['name']}")
                    ecosystem = "PyPI"
                entries.append((ecosystem, package["name"], package["version"]))
        result[file] = entries
    require(result, "no lockfiles found")
    return result


def check_policy(packages: dict, policy: dict) -> None:
    baseline = {file: names for file, names in policy.items() if file != EXCEPTIONS_KEY}
    require(set(packages) == set(baseline), "lockfile coverage changed; review and update dependency-policy.json")
    for file, entries in packages.items():
        names = {name for _, name, _ in entries}
        additions = names - set(baseline[file])
        require(not additions, f"{file}: unreviewed dependencies: {', '.join(sorted(additions))}")


def advisory_exceptions(policy: dict, locked: set[str]) -> list[dict]:
    """Validate, print and return the reviewed advisory exception list.

    An exception scopes exactly one advisory id to one locked package, with a
    written review reason and an expiry date. Every entry is printed so the
    job log records what was accepted and until when. An entry past its
    expiry fails the check: an exception is a time-limited review decision
    that is renewed or removed, never standing policy.
    """
    entries = policy.get(EXCEPTIONS_KEY, [])
    require(isinstance(entries, list), f"{EXCEPTIONS_KEY} in dependency-policy.json must be a list")
    exceptions = []
    seen = set()
    expired = []
    today = date.today()
    for entry in entries:
        require(isinstance(entry, dict), f"{EXCEPTIONS_KEY}: each entry must be an object")
        require(set(entry) == {"id", "package", "reason", "expires"},
                f"{EXCEPTIONS_KEY}: an entry carries exactly id, package, reason and expires")
        advisory, package, reason, expires = entry["id"], entry["package"], entry["reason"], entry["expires"]
        require(isinstance(advisory, str) and re.fullmatch(r"[A-Za-z]+-[0-9]{4}-[0-9A-Za-z-]+", advisory),
                f"{EXCEPTIONS_KEY}: {advisory!r} is not an advisory id such as RUSTSEC-2026-0346")
        require(isinstance(package, str) and package in locked,
                f"{EXCEPTIONS_KEY}: {advisory} names {package!r}, which is not in the locked dependency inventory")
        require(isinstance(reason, str) and reason.strip(),
                f"{EXCEPTIONS_KEY}: {advisory} on {package} needs the reason it was accepted")
        require(isinstance(expires, str), f"{EXCEPTIONS_KEY}: {advisory} expiry must be an ISO date (YYYY-MM-DD)")
        try:
            end = date.fromisoformat(expires)
        except ValueError:
            raise ValueError(f"{EXCEPTIONS_KEY}: {advisory} expiry must be an ISO date (YYYY-MM-DD)") from None
        require((advisory, package) not in seen, f"{EXCEPTIONS_KEY}: duplicate exception for {advisory} on {package}")
        seen.add((advisory, package))
        print(f"Advisory exception {advisory} on {package}: {reason.strip()} (expires {expires}; "
              f"{'EXPIRED' if end < today else 'active'})")
        if end < today:
            expired.append(f"{advisory} on {package} expired {expires}")
        exceptions.append({"id": advisory, "package": package, "expires": end})
    require(not expired, "an advisory exception is past its expiry (" + "; ".join(expired)
            + "); remove it, fix forward, or record a renewed review with a new expiry")
    return exceptions


def audit(packages: list[tuple[str, str, str]]) -> list[tuple[str, str, str, set[str]]]:
    findings = []
    for start in range(0, len(packages), 100):
        batch = packages[start:start + 100]
        queries = [{"package": {"ecosystem": eco, "name": name}, "version": version}
                   for eco, name, version in batch]
        request = urllib.request.Request(
            "https://api.osv.dev/v1/querybatch", data=json.dumps({"queries": queries}).encode(),
            headers={"Content-Type": "application/json", "User-Agent": "tungsten-dependency-audit"},
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            results = json.load(response)["results"]
        require(len(results) == len(batch), "OSV returned an incomplete batch")
        for package, result in zip(batch, results, strict=True):
            require(isinstance(result, dict) and not result.get("error"), "OSV returned a failed query")
            # A paginated result is already positive; fail closed rather than
            # claim the truncated response exhausts the advisory database.
            require(not result.get("next_page_token"), f"OSV truncated results for {package}")
            for vuln in result.get("vulns", []):
                # Aliases matter: OSV can key one advisory under a GHSA id
                # with the RUSTSEC id only as an alias, and exceptions are
                # recorded under whichever id the review cited.
                ids = {vuln["id"], *vuln.get("aliases", [])}
                findings.append((*package, ids))
    return findings


def apply_exceptions(findings: list[tuple[str, str, str, set[str]]],
                     exceptions: list[dict]) -> tuple[list, list, list[bool]]:
    """Suppress only findings an exception scopes to; report which were used."""
    matched = [False] * len(exceptions)
    remaining = []
    suppressed = []
    for finding in findings:
        hits = [position for position, exception in enumerate(exceptions)
                if exception["package"] == finding[1] and exception["id"] in finding[3]]
        if hits:
            suppressed.append(finding)
            for position in hits:
                matched[position] = True
        else:
            remaining.append(finding)
    return remaining, suppressed, matched


def print_finding(finding: tuple[str, str, str, set[str]]) -> None:
    ecosystem, name, version, ids = finding
    primary, *aliases = sorted(ids)
    also = "".join(f" (also {alias})" for alias in aliases)
    print(f"{ecosystem} {name}@{version}: https://osv.dev/vulnerability/{primary}{also}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    files = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
    check_manifests(root, files)
    packages = inventory(root, files)
    policy = json.loads((root / ".github/dependency-policy.json").read_text(encoding="utf-8"))
    check_policy(packages, policy)
    unique = sorted({entry for entries in packages.values() for entry in entries})
    for file, entries in packages.items():
        print(f"{file}: {len(entries)} locked external packages")
    exceptions = advisory_exceptions(policy, {name for _, name, _ in unique})
    if args.offline:
        print("Source/integrity/allowlist checks passed; advisory checks SKIPPED (--offline).")
        return 0
    findings = audit(unique)
    remaining, suppressed, matched = apply_exceptions(findings, exceptions)
    for position, exception in enumerate(exceptions):
        if not matched[position]:
            print(f"Advisory exception {exception['id']} on {exception['package']} matched no advisory; "
                  "remove it or narrow it.")
    for finding in remaining:
        print_finding(finding)
    print(f"OSV: {len(unique)} distinct locked package versions, {len(findings)} advisory matches, "
          f"{len(suppressed)} suppressed by reviewed exceptions.")
    return 1 if remaining else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"Dependency audit incomplete: {error}", file=sys.stderr)
        sys.exit(2)
