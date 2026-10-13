## Summary

<!-- What does this change and why? Link the issue: Fixes #123 -->

## Changes

-

## Release

<!-- Optional: suggest a release target if you have a preference. The maintainer
assigns and updates release labels; contributors do not need label permissions.
Leave this section empty if you have no preference. -->

## Checklist

- [ ] `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass
- [ ] `python3 .github/scripts/repository_guard.py --repo . --range origin/main HEAD` passes
- [ ] No tests, test data, `[dev-dependencies]` or doctests added (tests are maintained in the private repository)
- [ ] New source files carry the SPDX header (AGPL-3.0-only in `crates/`, Apache-2.0 in `runtimes/`)
- [ ] Commit messages carry no AI attribution trailers
- [ ] `CHANGELOG.md` updated for user-visible changes

A maintainer will run the private test suite; its result shows as the `tungsten-ops/ci` status.
