# Security policy

## Supported versions

tungsten is pre-1.0. Only the latest commit on `main` receives security fixes.
Once releases are tagged, this table will list the supported release lines.

| Version | Supported |
|---|---|
| `main` (latest) | yes |
| anything older | no |

## Reporting a vulnerability

Please do not open a public issue for a security problem.

Use GitHub's private vulnerability reporting: open the **Security** tab of this
repository and choose **Report a vulnerability** (a GitHub Security Advisory
draft that only the maintainers can see). If that option is not available to
you, contact the maintainer [@pboachie](https://github.com/pboachie) through
GitHub and ask for a private channel, without describing the problem in public.

Please include:

- the affected component (compiler, a runtime, the generated CLI, the MCP
  server, the mock server);
- the commit or version, and how you built it;
- steps or an OpenAPI document that reproduce the problem, and its impact.

## What to expect

These are targets for a small project, not guarantees:

| Step | Target |
|---|---|
| Acknowledgement of your report | within 3 business days |
| Initial assessment and severity | within 7 days |
| Fix or mitigation for confirmed high-severity issues | within 30 days |
| Public advisory | after a fix is available, coordinated with you |

We will credit you in the advisory unless you prefer to stay anonymous.

## Dependency controls

Run `python3 .github/scripts/dependency_audit.py` before merging dependency
changes. It checks every committed Cargo, npm and uv lockfile and the Python
release-tool lock against the package-name baseline in
`.github/dependency-policy.json`, approved registry
origins and artifact hashes, then queries OSV for the locked versions without
installing packages. Network failures fail the check. `--offline` checks only
local policy and must not be reported as a vulnerability scan.

New direct or transitive package names require an explicit baseline change and
maintainer review of necessity, upstream maintenance, licence, install/build
code and alternatives already in the standard library or dependency graph.
Remove unused names from the baseline when removing a dependency. Version
updates retain the existing package names and still receive advisory checks.
The baseline is an inventory requiring review, not an endorsement of safety.
Keep npm lifecycle scripts disabled during installation and use committed
lockfiles for CI builds. Cargo build scripts and proc macros still execute
during builds and must be treated as dependency code.

These checks cover the committed dependency resolution, not arbitrary future
consumer resolutions, language toolchains, native system libraries or unknown
vulnerabilities. Python release tools are installed from the hash-locked
`.github/packaging-requirements.lock`; release builds disable build isolation
so the build frontend cannot resolve additional unreviewed tools.
Generated SDK dependency changes also require emitter and runtime review.

An advisory that is reviewed and accepted for a limited time is recorded as a
scoped, expiring exception in the `advisory_exceptions` list of
`.github/dependency-policy.json`. Each entry names exactly the advisory id
(such as `RUSTSEC-2026-0346`) and the locked package it applies to, the reason
it is acceptable, and an `expires` date (`YYYY-MM-DD`). The audit prints every
entry in the job log, suppresses only the matching advisory for the named
package, and fails once an entry is past its expiry: an exception is a
time-limited review decision that is fixed forward, renewed or removed, not
standing policy. There is no package-wide, undated or blanket ignore.

## Release integrity

Release tags are immutable. A `vX.Y.Z` tag that has been pushed is never
moved, re-pointed, or deleted and re-created: build provenance attestations,
the release job's `--verify-tag` check and everyone who fetched the first
commit all key on that exact tag. A failed or incomplete release is fixed
forward — diagnose it with a `workflow_dispatch` dry run of the release
workflow, change the lockfile or code, bump the version and tag a new
release — never by moving the published tag.

## Project manifests and external emitters

A `tungsten.yml` is trusted input in one respect: a target with an `external`
key names a command that tungsten starts as a process, with the privileges of
the user running tungsten and no sandbox. The command runs on every command
that produces output in memory, not only on a writing `generate`: `generate
--dry-run` and `--check`, `check --ci`, `diff` and `report`. Plain `check`,
`ir dump` and `explain` never run it. A CI job that runs `tungsten check --ci`
on pull requests therefore executes whatever command a pull request puts in
`tungsten.yml`, the same trust model as a Makefile.

To validate a manifest you do not trust, pass `--no-external` or set
`TUNGSTEN_NO_EXTERNAL=1`: external targets are then skipped with a TG0807
notice and no command is started.

## Scope

In scope: the compiler (`crates/`), the runtime libraries (`runtimes/`), and
code that tungsten generates when the flaw comes from tungsten's emitters (for
example a generated client that sends credentials to the wrong host or an MCP
server that bypasses its sandbox or confirmation rules).

Out of scope: vulnerabilities in the API described by an OpenAPI document,
issues in third-party dependencies that are already tracked upstream (report
those upstream; tell us if tungsten is exposed in a way that needs a patch),
and denial of service from inputs that are documented as unsupported.
