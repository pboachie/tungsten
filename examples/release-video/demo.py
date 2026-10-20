# SPDX-License-Identifier: AGPL-3.0-only
"""Visible, deterministic helper commands used inside the recorded terminal."""
from pathlib import Path
import json
import os
import sys


def configure(repo, project, settings):
    repo = Path(repo)
    project = Path(project)
    s = json.loads(Path(settings).read_text(encoding="utf-8"))
    source = Path(s["source"])
    origin = json.loads(
        (repo / "sdk/tungsten/tungsten.yml").read_text(encoding="utf-8")
    )
    public = next(v for v in origin["inputs"] if v["namespace"] == "public")
    cfg = {
        "tungsten": 1,
        "api": {"name": "zrotext", "title": "ZROtext public API"},
        "inputs": [
            {
                **public,
                "spec": os.path.relpath(
                    repo / "protocol/v1/openapi/public-v1.json", project
                ).replace("\\", "/"),
            }
        ],
        "pagination": {
            k: v
            for k, v in origin.get("pagination", {}).items()
            if k.startswith("public.")
        },
        "auth_profiles": {
            k: v
            for k, v in origin.get("auth_profiles", {}).items()
            if k in ["ownerSession", "bearerAuth"]
        },
        "targets": {},
    }
    for target in ["typescript", "python", "rust", "java", "csharp", "ruby", "docs"]:
        cfg["targets"][target] = {"out": "generated/" + target}
    cfg["targets"]["typescript"].update(package="zrotext-sdk", runtime=s["runtime"])
    for target in ["java", "csharp", "ruby"]:
        cfg["targets"][target]["runtime_path"] = (
            source / "runtimes" / target
        ).as_posix()
    cfg["targets"]["mcp"] = {
        "out": "generated/mcp",
        "sdk_path": "../typescript",
        "runtime": s["runtime"],
        "sdk_runtime": s["runtime"],
    }
    cfg["targets"]["webmcp"] = {
        "out": "generated/webmcp",
        "sdk_path": "../typescript",
        "runtime": s["runtime"],
        "sdk_runtime": s["runtime"],
    }
    cfg["targets"]["go"] = {
        "out": "generated/go",
        "external": Path(s["go_emitter"]).as_posix(),
        "module": "example.com/zrotext-sdk-go",
        "package": "zrotext",
        "runtime_path": (source / "runtimes/go").as_posix(),
    }
    (project / "tungsten.yml").write_text(
        json.dumps(cfg, indent=2) + "\n", encoding="utf-8"
    )
    print("Configured all seven SDK languages from the native public OpenAPI contract:")
    print("  TypeScript, Python, Rust, Java, C#, Ruby, Go (external emitter).")
    print("Also: Rust CLI source, MCP server, experimental WebMCP adapter, docs.")
    print(
        "Six planned routes remain non-callable. No live-operation approval is granted."
    )


def summary(project):
    project = Path(project)
    cfg_path = project / "tungsten.yml"
    try:
        cfg = json.loads(cfg_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        cfg = None
    root = project / "generated"
    for folder in sorted(root.iterdir() if root.exists() else []):
        if folder.is_dir():
            manifest = folder / ".tungsten/manifest.json"
            count = len(
                [
                    p
                    for p in folder.rglob("*")
                    if p.is_file()
                    and "node_modules" not in p.parts
                    and ".tungsten" not in p.parts
                    and "dist" not in p.parts
                ]
            )
            print(
                f"{folder.name:12} generated source: {count} files; compiler manifest: {manifest.is_file()}"
            )
    print(
        "Source generation does not establish live API acceptance or every-language build parity."
    )


def imported(project):
    root = Path(project)
    spec = json.loads((root / "imported.openapi.json").read_text(encoding="utf-8"))
    print("Normalized contract:", spec["openapi"])
    for route, methods in spec["paths"].items():
        for method, op in methods.items():
            if method in ["get", "post", "put", "patch", "delete"]:
                print(
                    method.upper(),
                    route,
                    "imported=" + str(op.get("x-tungsten-imported", False)),
                )
    report = root / "import-report.json"
    if report.is_file():
        d = json.loads(report.read_text(encoding="utf-8"))
        print("Import report retained:", report.as_posix())
        print(
            "Review authentication, origins, safety, inferred types and completeness before live use."
        )


def inventory(path):
    d = json.loads(Path(path).read_text(encoding="utf-8"))
    print("EXACT RELEASE:", d["version"], "SOURCE:", d["source_commit"])
    print(
        "SDK languages: TypeScript | Python | Rust | Java | C# | Ruby | Go (external)"
    )
    print("Other outputs: Rust CLI | MCP | docs | experimental browser WebMCP")
    print(
        "Native OpenAPI: seven SDK languages. Imported formats: TypeScript/Python/Rust + CLI/MCP/WebMCP/docs."
    )
    print("Imported Java/C#/Ruby/Go are refused in this version (TG0702).")
    print(
        "Inputs: OpenAPI JSON/YAML, Swagger 2, Postman 2.1, cURL, .http/.rest, HAR, endpoint manifest, manual examples."
    )
    print(
        "Git/HTTP retrieval is external in v0.1.2. Local files feed the compiler; remote $ref is unsupported."
    )


def inspect_source(project, kind):
    root = Path(project) / "generated" / kind
    if kind == "rust":
        file = root / "zrotext-cli/src/main.rs"
        print(
            "Rust CLI source only; no Rust compiler or executable is used in this walkthrough."
        )
        print(file.as_posix())
        print("\n".join(file.read_text(encoding="utf-8").splitlines()[:17]))
    elif kind == "webmcp":
        p = json.loads((root / "package.json").read_text(encoding="utf-8"))
        print("Experimental browser WebMCP package:", p["name"])
        print("Exports:", ", ".join(p.get("exports", {})))
        print("Generated source only. No browser availability or integration claim.")
    else:
        print("Generated docs:")
        for file in sorted(root.iterdir()):
            if file.is_file():
                print(" ", file.name, file.stat().st_size, "bytes")


if __name__ == "__main__":
    action = sys.argv[1]
    if action == "configure":
        configure(*sys.argv[2:])
    elif action == "summary":
        summary(sys.argv[2])
    elif action == "imported":
        imported(sys.argv[2])
    elif action == "inventory":
        inventory(sys.argv[2])
    elif action == "copy-local":
        import shutil, hashlib

        source = Path(sys.argv[2]) / "protocol/v1/openapi/public-v1.json"
        dest = Path(sys.argv[3])
        shutil.copy2(source, dest)
        print("Local OpenAPI file:", dest.as_posix())
        print("SHA256:", hashlib.sha256(dest.read_bytes()).hexdigest())
        print("A local document can be used independently of Git.")
    elif action == "show":
        print(Path(sys.argv[2]).read_text(encoding="utf-8"))
    elif action == "manual":
        root = Path(sys.argv[2])
        spec = json.loads((root / "inferred.openapi.json").read_text(encoding="utf-8"))
        print(
            "Manual method/endpoint inference:",
            spec["openapi"],
            "paths:",
            ", ".join(spec["paths"]),
        )
        print(
            "Retained inference-report.json; example body was an actual controlled public GET response."
        )
    elif action == "cli":
        inspect_source(sys.argv[2], "rust")
    elif action == "webmcp":
        inspect_source(sys.argv[2], "webmcp")
    elif action == "docs":
        inspect_source(sys.argv[2], "docs")
    elif action == "rerun":
        print("Manual next-release workflow (run outside this active capture):")
        print(
            "  python release_video.py --version vNEXT --run-id review-next --stage audit"
        )
        print(
            "  Review/update profile.json against the exact release tag and capability probes."
        )
        print(
            "  python release_video.py --version vNEXT --run-id review-next --stage all"
        )
        print(
            "  Inspect QA, MP4 chapters, captions and provenance; human review before publication."
        )
    elif action == "boundaries":
        print(
            "Human review before publication. No publishing command or scheduled job exists in this pipeline."
        )
        print(
            "Source-only: Python, Rust CLI, Java, C#, Ruby, experimental WebMCP. Build checks: TypeScript, MCP, Go."
        )
        print(
            "No ZROtext gateway calls or SMS sends. One unsupported if/then/else constraint is reported."
        )
    else:
        raise SystemExit("Unknown action")
