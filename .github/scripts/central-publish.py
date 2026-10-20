#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Sign frozen Maven artifacts and optionally publish one Central deployment."""

from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.util
import json
import os
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid
import zipfile
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "native_artifacts", Path(__file__).with_name("native-runtime-artifacts.py")
)
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)
API = "https://central.sonatype.com/api/v1/publisher"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        raise ValueError(
            "Central API redirected; refusing to forward publishing credentials"
        )


def required(name):
    value = os.environ.get(name, "")
    if not value:
        raise ValueError(
            "required signing/publishing configuration is missing: " + name
        )
    return value


def gpg(home, args, data=None):
    result = subprocess.run(
        ["gpg", "--homedir", str(home), "--batch", "--no-tty", *args],
        input=data,
        capture_output=True,
        check=False,
    )
    if result.returncode:
        raise ValueError("GPG operation failed; inspect signing configuration locally")
    return result.stdout


def sign_bundle(directory, version, output, package="java"):
    # Validate untrusted downloaded artifacts before reading secrets.
    inventory = native.verify(directory, package, version)
    if output.exists() or output.is_symlink():
        raise ValueError("Central output must not already exist")
    output.parent.mkdir(parents=True, exist_ok=True)
    fingerprint = required("MAVEN_GPG_FINGERPRINT").upper()
    if not re.fullmatch(r"[0-9A-F]{40}", fingerprint):
        raise ValueError("expected one full signing-key fingerprint")
    with tempfile.TemporaryDirectory(prefix="tungsten-central-") as scratch:
        home = Path(scratch) / "gnupg"
        home.mkdir(mode=0o700)
        try:
            gpg(home, ["--import"], required("MAVEN_GPG_PRIVATE_KEY").encode())
            keys = (
                gpg(home, ["--with-colons", "--list-secret-keys"]).decode().splitlines()
            )
            primaries = []
            waiting = False
            for line in keys:
                kind = line.split(":")[0]
                if kind == "sec":
                    waiting = True
                elif kind == "fpr" and waiting:
                    primaries.append(line.split(":")[9])
                    waiting = False
            if primaries != [fingerprint]:
                raise ValueError(
                    "imported signing key differs from the reviewed fingerprint"
                )
            entries = {}
            artifacts = native.maven_artifacts(package)
            for name in sorted(inventory["files"]):
                artifact = next(
                    a for a in artifacts if name.startswith(f"{a}-{version}")
                )
                prefix = f"io/github/pboachie/tungsten/{artifact}/{version}/"
                path = directory / name
                signature = Path(scratch) / (name + ".asc")
                gpg(
                    home,
                    [
                        "--pinentry-mode",
                        "loopback",
                        "--passphrase-fd",
                        "0",
                        "--local-user",
                        fingerprint,
                        "--armor",
                        "--detach-sign",
                        "--output",
                        str(signature),
                        str(path),
                    ],
                    (required("MAVEN_GPG_PASSPHRASE") + "\n").encode(),
                )
                status = gpg(
                    home, ["--status-fd", "1", "--verify", str(signature), str(path)]
                ).decode()
                if not any(
                    line.startswith("[GNUPG:] VALIDSIG ")
                    and fingerprint in line.split()
                    for line in status.splitlines()
                ):
                    raise ValueError(
                        "signature does not verify against the reviewed signing key"
                    )
                entries[prefix + name] = native.regular(path)
                entries[prefix + name + ".asc"] = native.regular(signature)
                for algorithm in ("md5", "sha1", "sha256", "sha512"):
                    entries[prefix + name + "." + algorithm] = (
                        hashlib.new(algorithm, entries[prefix + name])
                        .hexdigest()
                        .encode()
                    )
            # Recheck frozen inputs after signing; no Maven lifecycle runs here.
            native.verify(directory, package, version)
            with zipfile.ZipFile(
                output, "x", compression=zipfile.ZIP_DEFLATED
            ) as archive:
                for name, data in sorted(entries.items()):
                    archive.writestr(name, data)
        finally:
            subprocess.run(
                ["gpgconf", "--homedir", str(home), "--kill", "gpg-agent"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
    return inventory


def request(path, token, body=b"", content_type="application/json"):
    req = urllib.request.Request(
        API + path,
        data=body,
        method="POST",
        headers={
            "Authorization": "Bearer " + token,
            "Content-Type": content_type,
            "User-Agent": "tungsten-release-workflow",
        },
    )
    try:
        opener = urllib.request.build_opener(NoRedirect())
        with opener.open(req, timeout=60) as response:
            data = response.read(1024 * 1024 + 1)
            if len(data) > 1024 * 1024:
                raise ValueError("Central response exceeded the size limit")
            return data
    except urllib.error.HTTPError as error:
        # Response bodies can contain sensitive server diagnostics.
        raise ValueError(
            f"Central request returned HTTP {error.code}; inspect the Portal before retrying"
        ) from None
    except urllib.error.URLError:
        raise ValueError(
            "Central request outcome is uncertain; inspect the Portal before retrying"
        ) from None


def publish(bundle, version, receipt, timeout=1200, package="java"):
    if receipt.exists() or receipt.is_symlink():
        raise ValueError(
            "deployment receipt already exists; inspect it before retrying"
        )
    token = base64.b64encode(
        (
            required("MAVEN_CENTRAL_USERNAME")
            + ":"
            + required("MAVEN_CENTRAL_PASSWORD")
        ).encode()
    ).decode()
    boundary = "tungsten-" + uuid.uuid4().hex
    data = native.regular(bundle)
    body = (
        (
            f'--{boundary}\r\nContent-Disposition: form-data; name="bundle"; filename="bundle.zip"\r\n'
            "Content-Type: application/zip\r\n\r\n"
        ).encode()
        + data
        + f"\r\n--{boundary}--\r\n".encode()
    )
    # Never retry this write automatically: an uncertain response may have published.
    result = request(
        f"/upload?publishingType=AUTOMATIC&name={native.PACKAGES[package]}-{version}",
        token,
        body,
        "multipart/form-data; boundary=" + boundary,
    )
    deployment = result.decode().strip().strip('"')
    try:
        uuid.UUID(deployment)
    except ValueError:
        raise ValueError(
            "Central did not return a deployment ID; inspect the Portal before retrying"
        ) from None
    receipt.parent.mkdir(parents=True, exist_ok=True)
    summary = {
        "deployment_id": deployment,
        "version": version,
        "bundle_sha256": hashlib.sha256(data).hexdigest(),
        "state": "UPLOADED",
    }
    receipt.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = json.loads(request("/status?id=" + deployment, token))
        state = status.get("deploymentState")
        if state not in {
            "PENDING",
            "VALIDATING",
            "VALIDATED",
            "PUBLISHING",
            "PUBLISHED",
            "FAILED",
        }:
            raise ValueError("Central returned an unknown deployment state")
        summary["state"] = state
        receipt.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
        if state == "FAILED":
            raise ValueError(
                "Central rejected the deployment; inspect its validation details in the Portal"
            )
        if state == "PUBLISHED":
            expected = sorted(
                f"pkg:maven/io.github.pboachie.tungsten/{artifact}@{version}"
                for artifact in native.maven_artifacts(package)
            )
            purls = status.get("purls")
            if not isinstance(purls, list) or sorted(purls) != expected:
                raise ValueError("Central published an unexpected component inventory")
            return
        time.sleep(20)
    raise ValueError(
        "Central publication is still pending; inspect the recorded deployment before retrying"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument(
        "--package",
        choices=("java", "kotlin"),
        default="java",
        help="Maven component set to sign and publish (one Central deployment each)",
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, default=Path("central-deployment.json"))
    parser.add_argument(
        "--publish",
        action="store_true",
        help="perform the irreversible Central upload after signing",
    )
    args = parser.parse_args()
    try:
        sign_bundle(
            args.directory.absolute(),
            args.version,
            args.output.absolute(),
            args.package,
        )
        if args.publish:
            publish(args.output, args.version, args.receipt, package=args.package)
    except (
        OSError,
        ValueError,
        KeyError,
        TypeError,
        zipfile.BadZipFile,
        subprocess.SubprocessError,
    ) as error:
        parser.exit(1, f"Central publication: {error}\n")
    print(
        "Central deployment published"
        if args.publish
        else "Signed Central bundle prepared; nothing uploaded"
    )


if __name__ == "__main__":
    main()
