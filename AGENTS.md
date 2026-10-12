# Agent instructions

Read [CONTRIBUTING.md](CONTRIBUTING.md) before changing this repository.

## Release target required on every pull request

- Follow [.github/RELEASES.md](.github/RELEASES.md). The approved plan is
  [.github/release-plan.json](.github/release-plan.json) on the current remote
  default branch. Read that version before opening, updating or handing off a
  PR; a worktree's copy may be stale.
- Every PR, including drafts, documentation, dependency and infrastructure
  changes, must have exactly one `release:X.Y.Z` GitHub label. This is the
  intended tungsten release, not a Git tag or the package version on the branch.
- Use the plan's `default_target` for new work. For example, published `0.1.1`
  normally means target `0.1.2`; merging a version-bump PR does not mean
  `0.1.2` has shipped. Never independently increment versions per PR.
- Verify the published stable release against the plan. Resolve stale or
  conflicting state with the listed coordinator before assigning a new target
  or merging; continue unrelated implementation work meanwhile.
- Set `Release target: X.Y.Z` in the PR description, apply the matching label,
  and verify both before handoff. Preserve unrelated labels. If label access is
  unavailable, report that specific blocker; do not claim labeling is complete.
- Keep an existing approved target when the default advances. Only the release
  coordinator can approve retargeting, new version lines or frozen-scope changes.
  Record those decisions in the tracking PR and update the plan through a PR.
- Ordinary feature/fix PRs do not bump package versions. Coordinate with the
  release PR owner before changing manifests, locks or release notes. Creating
  a label or updating the plan does not authorize a release tag or publication.

## Repository boundaries and validation

- Inspect the working tree and existing PRs before editing; preserve other
  contributors' changes and avoid duplicating owned work.
- Public source, community files and these agent instructions live here.
  Tests, fixtures, detailed plans and operational evidence belong in the private
  operations repository. Do not copy private material into a public PR.
- Follow the SPDX, runtime parity and additive IR rules in CONTRIBUTING.md.
  Keep changes focused and update `CHANGELOG.md` for user-visible behavior.
- Run the relevant contribution checks and report their actual results.
  Local checks, required CI, private-suite results and published release
  verification are separate evidence; one does not establish the others.
- Follow the repository's commit-attribution rules. Do not merge or publish
  unless the user has authorized that action.
