## Summary

<!-- What does this change and why? Link the issue: Fixes #123 -->

## Changes

-

## Release

Release target: X.Y.Z

<!-- Read .github/release-plan.json on the remote default branch. Replace X.Y.Z
with the approved target and apply exactly one matching release:X.Y.Z label.
Follow .github/RELEASES.md; do not derive it from a branch's package version.
For a target change, link the coordinator's decision in the tracking PR. -->

## Checklist

- [ ] Live release plan checked; exactly one `release:X.Y.Z` label matches the target above
- [ ] Version bumps or frozen-scope changes, if any, are coordinated in the release tracking PR
- [ ] `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass
- [ ] `python3 .github/scripts/repository_guard.py --repo . --range origin/main HEAD` passes
- [ ] No tests, test data, `[dev-dependencies]` or doctests added (tests are maintained in the private repository)
- [ ] New source files carry the SPDX header (AGPL-3.0-only in `crates/`, Apache-2.0 in `runtimes/`)
- [ ] Commit messages carry no AI attribution trailers
- [ ] `CHANGELOG.md` updated for user-visible changes

A maintainer will run the private test suite; its result shows as the `tungsten-ops/ci` status.
