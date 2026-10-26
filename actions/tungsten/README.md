# tungsten GitHub Action

A composite action that installs a pinned tungsten release and checks a
project: the contract (`tungsten check`), the committed generated output
(`tungsten generate --check`) and the generation lock (`tungsten lock
--check`). It fails the job with the tungsten diagnostics in the log and a
summary table on the run page.

```yaml
permissions:
  contents: read

jobs:
  tungsten:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: pboachie/tungsten/actions/tungsten@<commit> # <tag>
        with:
          version: 0.1.3
```

Replace `<commit>` with the full commit SHA of a tungsten release tag and keep
the tag in the comment. A complete workflow is in
[`examples/github-actions/tungsten-action.yml`](../../examples/github-actions/tungsten-action.yml).
This action is consumed from this repository by path; it is not published on
the GitHub Marketplace. No release tag contains it yet: until one does, pin a
`main` commit that contains `actions/tungsten`.

The `examples` workflow of this repository runs the action from its checkout
on Linux x86_64, Linux aarch64 and macOS against a small consumer project: an
unchanged contract passes, a stale output and an invalid manifest fail with
the expected `failed` output, and the regenerated project passes.

## Inputs

| Input | Default | Meaning |
|---|---|---|
| `version` | required | Release to install, for example `0.1.3`. There is no "latest". |
| `projects` | `.` | Projects to check, one per line: a directory with `tungsten.yml` or a `tungsten*.yml` manifest. Checked in the order given; every listed project is checked even after a failure. |
| `workspace` | empty | Instead of `projects`: a workspace root (relative to `working-directory`). The projects `tungsten workspace` discovers there are checked in its discovery order. Setting both `workspace` and a non-default `projects` fails. |
| `workspace-projects` | empty | With `workspace`: only these project ids, one per line (`--project`; an unknown id fails with `TG0644`). |
| `affected-files` | empty | With `workspace`: only the projects that depend on one of these files, one per line (`--affected`). tungsten decides the dependency from the manifest, its declared inputs and the files its offline compilation read; a project that does not compile counts as affected by every file. |
| `affected-base` | empty | With `workspace`: also treat the files changed in `git diff <base>...HEAD` (both sides of a rename) as affected, for example `${{ github.event.pull_request.base.sha }}`. The base must be in the checkout (`fetch-depth: 0`). When affected files are requested and nothing changed, no project is checked and the step passes. |
| `working-directory` | `.` | Directory the project paths are relative to. Project paths must stay inside it (no absolute paths, no `..`). |
| `check` | `true` | Run `tungsten check`. |
| `generate-check` | `true` | Run `tungsten generate --check`: writes nothing, exits 1 with `TG0901` when a generated file is missing, changed or extra. |
| `lock-check` | `auto` | Run `tungsten lock --check`: `auto` runs it when the project directory has a `tungsten.lock.json`; `true` always (a missing lock fails); `false` never. `tungsten lock` is newer than 0.1.3: keep `auto` (without a lock) or `false` for that release. |
| `locked` | `false` | Pass `--locked` to check and generate (`TG0128` unless the project matches its lock; inputs are acquired offline). |
| `strict` | `false` | Pass `--strict` to check and generate: warnings fail. |
| `allow-external` | `false` | Start the commands of `external` targets. See the trust model. |
| `scenarios` | empty | Saved mock scenarios to run after the checks, one per line: `<scenario.json> [--expect-exit N] [--timeout-ms N] [--max-calls N] -- <command> [args...]`. Each runs `tungsten scenario run <scenario-input> --scenario <file> ... -- <command>`, the runner used locally; a failed assertion fails the step. Refused on `pull_request_target`. |
| `scenario-input` | `.` | The project, manifest or OpenAPI document the scenarios run against. |
| `install-only` | `false` | Install tungsten, add it to `PATH` and stop; run your own commands afterwards. |
| `token` | `github.token` | Token for the provenance check of the release archive. Only the install step sees it. |

Boolean inputs accept exactly `true` or `false`; anything else fails the step.

`workspace`, `workspace-projects` and the affected inputs need a release that
has `tungsten workspace` (newer than 0.1.3). Affected-project selection uses
tungsten's own dependency evidence, so a change to an input shared by two
projects checks both, and a change to one project's spec checks only that one:

```yaml
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
          fetch-depth: 0
      - uses: pboachie/tungsten/actions/tungsten@<commit> # <tag>
        with:
          version: <release>
          workspace: apis
          affected-base: ${{ github.event.pull_request.base.sha || github.event.before }}
```

On a push to a new branch `github.event.before` is all zeros; the action
treats an all-zeros base as no base and checks every selected project.

## Outputs

| Output | Meaning |
|---|---|
| `version` | The output of `tungsten --version`. |
| `bin-dir` | Directory of the installed binary (also on `PATH` for later steps). |
| `log-dir` | One log per check, `<n>-<check>.log` (`1-check.log`, `2-generate-check.log`, ...). Upload it with `actions/upload-artifact` when you want the logs outside the run page. |
| `projects` | The number of projects checked; 0 when a workspace selection matched none. |
| `failed` | The failed checks, one `<project>: <check>` per line; empty when all passed. |

Each check also prints in its own log group and, on failure, adds an error
annotation naming the project and the exit status.

## Scenarios

Each scenario line is split on whitespace and never passed through a shell:
quoting, `$VAR`, globs and redirections are not interpreted, and a word cannot
contain a space. Scenario files and the input must be relative paths inside
`working-directory`, and every line is validated before any check runs. The
command after `--` is code from your repository (an integration test, for
example) and runs with the job's environment, like any `run:` step; the
action does not pass the install token to it. tungsten bounds each run with
`--timeout-ms` (default 30 s) and `--max-calls`, and its report is the mock's
bounded summary. Scenarios need a release that has `tungsten scenario`
(newer than 0.1.3).

```yaml
      - uses: pboachie/tungsten/actions/tungsten@<commit> # <tag>
        with:
          version: <release>
          projects: openapi.json
          generate-check: "false"
          scenario-input: openapi.json
          scenarios: |
            scenarios/rate-limit.json -- python3 -I integration.py rate-limit
            scenarios/malformed.json --expect-exit 3 -- python3 -I integration.py malformed
```

## Installation and pinning

The action runs `install.sh` from the same commit as the action, so pinning
the action by commit SHA pins the installer as well. The installer downloads
`tungsten-<version>-<target>` from the GitHub release, checks it against the
release's `SHA256SUMS` and verifies its build provenance from tungsten's
release workflow for that tag with `gh attestation verify` (GitHub CLI 2.102.0
or newer, preinstalled on GitHub-hosted runners). The action clears
`TUNGSTEN_NO_ATTESTATION` and the mirror variables, so provenance verification
cannot be switched off from the workflow environment.

The compiler binary lands in `$RUNNER_TEMP`, outside the workspace. No
toolchain is needed to run the checks; building the generated packages is a
separate step with that target's own toolchain.

Supported runners: Linux x86_64 and aarch64 (static musl build) and macOS
(x86_64 and Apple silicon). Windows runners fail with a message; install
there with `install.ps1`.

The runner needs GitHub CLI 2.102.0 or newer for the provenance check. The
action checks this first and fails with a message when it is missing or
older; it never falls back to a checksum-only install. Some images ship an
older CLI (`macos-15` at the time of writing): update it in a step before the
action, for example `brew upgrade gh`.

To upgrade, change `version` and the action commit together, to the commit of
the new release tag, regenerate locally with that release, and review the
diff in one pull request. Dependabot's `github-actions` ecosystem can propose
the commit change.

## Fixing a failure

- `generate --check` (`TG0901`): run `tungsten generate` locally with the same
  release and commit the changed, added and removed files.
- `check`: the log shows each diagnostic with its code and location;
  `tungsten explain <code>` explains it.
- `lock --check`: review the drift with `tungsten lock --preview`, then accept
  it with `tungsten lock --update` and commit `tungsten.lock.json`.

## Trust model

On a pull request, every file the action reads can come from the pull
request, including `tungsten.yml`. An `external:` target names a command that
tungsten starts with the job's privileges. So:

- External commands are off by default (`TUNGSTEN_NO_EXTERNAL=1`, equivalent
  to `--no-external`); their targets are reported as skipped (`TG0807`) and
  their output is not checked.
- `allow-external: true` is ignored, with a warning, for a pull request from a
  fork and for any `pull_request_target` event. Use it only where every author
  of the checked refs is trusted, and pin and review the emitter yourself: it
  is unsandboxed code.
- Inputs reach the shell through environment variables, never by expression
  interpolation, so a branch name or input value cannot inject commands.
- The action needs `contents: read` only, uses no secrets and writes nothing
  to the repository. Use the `pull_request` trigger, not `pull_request_target`,
  and keep publishing or deploying in a separate workflow that runs on trusted
  refs only.
