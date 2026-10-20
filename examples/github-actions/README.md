# GitHub Actions: validate and check generated output

A workflow you copy into your own repository. On every pull request and push to
`main` it installs a pinned tungsten release, runs `tungsten check` and fails
when the committed generated output is stale.

```sh
mkdir -p .github/workflows
cp examples/github-actions/tungsten.yml .github/workflows/tungsten.yml
```

Set `TUNGSTEN_PROJECT` in the file to the directory of your `tungsten.yml` (the
default is the repository root) and commit the generated output of
`tungsten generate` next to it.

## What it runs

1. `actions/checkout`, without persisting the token.
2. The pinned installer (below), then `tungsten --version`.
3. `tungsten check`: validates the manifests and specs. A nonzero exit status
   fails the job, with the `TG` diagnostics in the log.
4. `tungsten generate --check`: compares the output directories with what
   `generate` would write, writes nothing and exits 1 with `TG0901` when they
   differ.

To fix a stale-output failure, run `tungsten generate` locally with the same
release and commit the changed files. Details of a diagnostic: `tungsten explain
TG0901`.

The runner is `ubuntu-24.04` (x86_64). The release archive is the static musl
build, so the job needs no compiler or runtime beyond what your generated
output needs when you build it. This workflow only runs the compiler. Windows
and macOS runners are not covered by this example.

## Pinning and upgrades

Everything the job downloads or runs is fixed:

- Actions are pinned by full commit SHA, with the release tag in a comment.
- The compiler is `TUNGSTEN_VERSION`. `install.sh` checks the archive against
  that release's `SHA256SUMS` and, by default, its build provenance with the
  GitHub CLI (2.102.0 or newer; preinstalled on GitHub-hosted runners). The
  workflow never sets `TUNGSTEN_NO_ATTESTATION`. Verification needs a GitHub
  token; the job passes `github.token` to that one step only.
- The installer itself is fetched from the tungsten repository at a commit
  (`TUNGSTEN_INSTALLER_URL`) and refused unless its SHA-256 matches
  `TUNGSTEN_INSTALLER_SHA256`. It is not read from `main`.

To upgrade, change `TUNGSTEN_VERSION`, the commit in `TUNGSTEN_INSTALLER_URL`
(the commit of the new release tag) and the checksum together, regenerate with
the new release, and review the diff in a pull request. Update action SHAs the
same way, from the tag's commit. Dependabot's `github-actions` ecosystem can
propose them.

## Trust model

A pull request can change any file the job reads, including `tungsten.yml`.
An `external:` target in that file names a command that tungsten starts with
your privileges, on `generate --check` and `check --ci` as well as on `generate`.
So the workflow:

- sets `TUNGSTEN_NO_EXTERNAL=1` for the whole job, equivalent to
  `--no-external`: external commands are never started and their targets are
  reported as skipped (`TG0807`), also on pull requests from forks;
- grants only `contents: read` and references no `secrets`; fork pull requests
  receive a read-only token and no secrets anyway;
- uses the `pull_request` trigger. Do not switch it to `pull_request_target`,
  and do not add secrets, deploy or publish steps to this job. Publish from a
  separate workflow that runs on trusted refs only;
- never puts event data (branch names, titles) into a shell script.

Targets that use an external emitter (for example Go) are therefore not
checked by this workflow; their output is reported as skipped. If every
contributor is trusted, remove the `TUNGSTEN_NO_EXTERNAL` line to run them, and
accept that the pull request's manifest then runs commands on the runner. The
emitter itself is unsandboxed code: pin and review it separately.
