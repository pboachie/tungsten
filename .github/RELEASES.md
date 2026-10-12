# Pull request release targets

Every PR has exactly one GitHub label named `release:X.Y.Z` and a matching
`Release target: X.Y.Z` line in its description. This includes drafts, bot PRs,
documentation and CI changes. The label expresses intended inclusion; it does
not prove that the change shipped. PR authors apply and verify it; maintainers
handle bot and external-contributor PRs. Reviewers check it before merge.

## Shared plan

[`release-plan.json`](release-plan.json) on the remote default branch is the
single approved plan. Do not use a stale feature-branch copy or infer a release
from a manifest bump. Read the live plan and latest published stable release:

```sh
gh api repos/pboachie/tungsten/contents/.github/release-plan.json -H "Accept: application/vnd.github.raw+json"
gh release view --repo pboachie/tungsten --json tagName,isDraft,isPrerelease,publishedAt
```

- `latest_stable` is the verified published stable version, without `v`.
  Drafts, prereleases, tags alone and bumped manifests do not advance it.
- `default_target` is the approved version for new PRs. It must name an `open`
  entry in `targets` and be newer than `latest_stable`.
- Each target has a coordinator (GitHub login), a tracking PR number, and a
  status of `open` or `frozen`. Versions use numeric `X.Y.Z`, without `v` or
  prerelease suffixes. RC work keeps the intended final stable version label.
- The tracking PR records scope, blockers, deferred work, version decisions
  and release evidence. Use the existing release PR; do not create competing
  release coordination threads. A new target may initially use its plan-change
  PR as the tracking PR, then switch to its release preparation PR.

If the published stable release and the plan disagree, the coordinator must
reconcile them before new target assignments or merges. If no approved plan
exists, propose the next patch after the latest stable release and obtain the
coordinator's decision through a plan PR. Do not silently invent a target.

## Applying the label

Read the plan immediately before creating a PR. Use `default_target` unless
the coordinator has explicitly assigned the work to another listed target.
For a plan targeting `0.1.2`, the commands are:

```sh
gh label create "release:0.1.2" --repo pboachie/tungsten --color 1D76DB --description "Intended for tungsten 0.1.2" --force
gh pr edit PR_NUMBER --repo pboachie/tungsten --add-label "release:0.1.2"
gh pr view PR_NUMBER --repo pboachie/tungsten --json labels,body
```

Replace the example version with the live approved target and `PR_NUMBER` with
the actual number. Add `Release target: 0.1.2` to the description as well. On an
approved retarget, add the new label and remove only obsolete `release:` labels
with `gh pr edit --remove-label`; preserve other labels. Verify exactly one
release label remains and the description agrees. Do not relabel an existing
PR merely because the default changed. Never overwrite a conflicting explicit
target without resolving it with the coordinator.

These requirements are an author/reviewer workflow, not an automatic GitHub
labeling job or a required status check. A PR checklist alone does not apply a
label. Agents must perform the GitHub edit and verify the result.

## Deciding when to change versions

The listed coordinator owns the decision; agents propose it in the tracking PR.
One release preparation PR owns manifest and lockfile bumps. Parallel feature
PRs use labels and `Unreleased` changelog entries without bumping versions.

- Patch (`0.1.1` to `0.1.2`): the default next release for compatible fixes,
  maintenance and the already agreed release scope.
- Minor (`0.1.x` to `0.2.0`): propose for a new feature release or changes to
  generated APIs, configuration or runtime contracts that require migration.
  Before `1.0`, breaking changes require a coordinated minor release.
- Major (`1.x` to `2.0.0`): breaking changes after `1.0`. The first `1.0.0`
  requires an explicit stability/readiness decision, not a patch-counter rule.

PR count, elapsed time, a green subset of CI, or one agent finishing work do
not trigger a version change. A release can proceed when its agreed scope is
merged or explicitly deferred, required checks pass on the exact candidate,
release blockers are resolved, and the coordinator authorizes the cut using
the existing release process. The label does not replace any release gate.

## Freeze and rollover

1. The coordinator records the candidate scope and unresolved blockers in the
   tracking PR. To freeze it, merge a plan change setting its status to
   `frozen`, adding the next approved `open` target, and moving `default_target`
   to that target. For example, freeze `0.1.2` and open `0.1.3`. This does not
   change `latest_stable` or publish anything.
2. Existing PRs keep their target. New work uses the new default. Only the
   coordinator may admit a blocker fix into frozen scope. Work for the next
   release must not merge into the frozen candidate branch until the release
   is cut, unless the coordinator has arranged a separate release branch.
3. Before the cut, inventory all open PRs labeled for the frozen version. Merge
   accepted work, close canceled work, or explicitly defer and retarget it.
   Record the reason in the tracking PR and update both labels and description
   for carried-over work. Do not leave open PRs targeting a shipped release.
4. Verify the stable release and required publication jobs/artifacts actually
   succeeded. A GitHub Release may exist before all publishing jobs finish.
   Failed or partial publication remains a release blocker; do not mark it done
   or create another version to hide it.
5. Through one reviewed plan PR, update `latest_stable`, remove the completed
   target from `targets`, and ensure an approved open default remains. Keep
   merged/closed PR labels and repository label definitions as history. Do not
   bulk-retarget historical PRs. Audit open PRs again after the plan merges.

If the coordinator changes an unreleased version (for example, `0.1.2` becomes
`0.2.0`), the plan PR must list the affected PRs and rationale. Relabel the
affected open PRs after approval; preserve historical assignments on closed
PRs and document the mapping in the tracking PR/release notes. After any plan
change, agents re-read the merged plan before assigning or merging work.
