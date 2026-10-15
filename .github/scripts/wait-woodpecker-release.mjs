// SPDX-License-Identifier: AGPL-3.0-only
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { setTimeout } from "node:timers/promises";
import { pathToFileURL } from "node:url";

// The controller writes commit statuses. Build containers need no dispatch or npm credential.
export function releaseStatus(statuses, sha, tag, repoId, serverOrigin) {
  const status = statuses.find(item => item.context === "ci/woodpecker/tag/npm-release");
  if (!status || status.state === "pending") return null;
  assert.equal(status.state, "success", "Woodpecker release validation failed");
  const url = new URL(status.target_url);
  const server = new URL(serverOrigin);
  assert(server.protocol === "https:" && server.href === `${server.origin}/`, "invalid Woodpecker server origin");
  assert.equal(url.origin, server.origin, "unexpected Woodpecker server");
  assert.match(repoId, /^\d+$/, "missing Woodpecker repository ID");
  const match = url.pathname.match(new RegExp(`^/repos/${repoId}/pipeline/(\\d+)(?:/\\d+)?$`));
  assert(match && !url.search && !url.hash && !url.username && !url.password, "unexpected pipeline URL");
  return { url: `${url.origin}/api/repos/${repoId}/pipelines/${match[1]}`, sha, tag };
}

export function verifyPipeline(pipeline, expected) {
  assert.equal(pipeline.status, "success", "Woodpecker pipeline did not succeed");
  assert.equal(pipeline.commit, expected.sha, "Woodpecker validated a different commit");
  assert.equal(pipeline.ref, `refs/tags/${expected.tag}`, "Woodpecker validated a different tag");
  assert.equal(pipeline.event, "tag", "Woodpecker run was not a tag event");
  const workflow = pipeline.workflows?.find(item => item.name === "npm-release");
  assert.equal(workflow?.state, "success", "npm release workflow did not succeed");
  assert.equal(workflow.children?.find(item => item.name === "validate-npm-artifacts")?.state,
    "success", "npm artifact validation step did not succeed");
}

// The v0.1.2 tag's first release run died here after 15 minutes while the
// external Woodpecker queue was still working, and npm publication needed a
// manual re-run about 20 minutes later. The wait now starts only after the npm
// artifacts it gates exist (the workflow job depends on npm-artifacts), so
// 45 minutes covers the observed queue plus validation run time with margin.
// The woodpecker-release job timeout is 50 minutes: this deadline plus
// checkout and setup.
export const WAIT_DEADLINE_MINUTES = 45;
const POLL_INTERVAL_MS = 15_000;
// A transient network error or rate limit during the wait must not fail the
// gate; only a validation verdict or the deadline ends it.
const FETCH_ATTEMPTS = 3;
const FETCH_RETRY_DELAY_MS = 10_000;

export function timeoutMessage(mode) {
  if (mode === "exercise") {
    return `Woodpecker validation timed out after ${WAIT_DEADLINE_MINUTES} minutes during the dry-run gate exercise: the last released tag has no finished receipt. Investigate the Woodpecker pipeline before the next release; this dry run published nothing.`;
  }
  return `Woodpecker validation timed out after ${WAIT_DEADLINE_MINUTES} minutes; a timeout is not a validation verdict. No npm publication happened, and the GitHub Release and attestations are unaffected. Re-run the failed jobs of this run once the Woodpecker pipeline finishes; npm publication is retried together with this gate.`;
}

async function fetchJson(url, token) {
  let lastError = new Error("no request was attempted");
  for (let attempt = 1; attempt <= FETCH_ATTEMPTS; attempt += 1) {
    try {
      const headers = { Accept: "application/vnd.github+json" };
      if (token) headers.Authorization = `Bearer ${token}`;
      const response = await fetch(url, { headers, signal: AbortSignal.timeout(30_000) });
      if (response.ok) return response.json();
      lastError = new Error(`lookup failed: ${response.status}`);
      // 429 and 5xx are transient; anything else (a bad token, a missing
      // pipeline) is final and retrying cannot change it.
      if (response.status !== 429 && response.status < 500) break;
    } catch (error) {
      lastError = error;
    }
    if (attempt < FETCH_ATTEMPTS) await setTimeout(FETCH_RETRY_DELAY_MS);
  }
  throw lastError;
}

// The dry-run gate exercise replays the wait against the newest released tag,
// which already carries a Woodpecker receipt, instead of the dispatch commit.
export function lastReleasedTag() {
  const tag = execFileSync("git", ["tag", "--list", "v*", "--sort=-v:refname"], { encoding: "utf8" })
    .split("\n").filter(Boolean).at(0);
  if (!tag) return null;
  const sha = execFileSync("git", ["rev-parse", `refs/tags/${tag}^{commit}`], { encoding: "utf8" }).trim();
  assert.match(sha, /^[0-9a-f]{40}$/);
  return { sha, tag };
}

async function main() {
  const mode = process.env.WOODPECKER_RELEASE === "true" ? "release"
    : process.env.WOODPECKER_EXERCISE === "true" ? "exercise" : null;
  if (!mode) {
    console.log("Woodpecker gate is disabled; using GitHub artifact validation.");
    return;
  }
  let sha = process.env.GITHUB_SHA;
  let tag = process.env.RELEASE_TAG;
  if (mode === "exercise") {
    const last = lastReleasedTag();
    if (!last) {
      console.log("No released tag exists yet; the Woodpecker gate exercise has nothing to replay.");
      return;
    }
    ({ sha, tag } = last);
    console.log(`Dry run: exercising the Woodpecker gate against the last released tag ${tag}.`);
  }
  assert.match(sha ?? "", /^[0-9a-f]{40}$/);
  assert.match(tag ?? "", /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/);
  const deadline = Date.now() + WAIT_DEADLINE_MINUTES * 60_000;
  while (Date.now() < deadline) {
    const statuses = await fetchJson(
      `https://api.github.com/repos/pboachie/tungsten/commits/${sha}/statuses?per_page=100`, process.env.GITHUB_TOKEN);
    const expected = releaseStatus(statuses, sha, tag,
      process.env.WOODPECKER_REPO_ID ?? "", process.env.WOODPECKER_URL ?? "");
    if (expected) {
      const pipeline = await fetchJson(expected.url);
      verifyPipeline(pipeline, expected);
      console.log(`${mode === "release" ? "Woodpecker validated" : "Dry run verified the Woodpecker receipt for"} ${tag} at ${sha}`);
      return;
    }
    console.log("Waiting for the Woodpecker npm release validation...");
    await setTimeout(POLL_INTERVAL_MS);
  }
  throw new Error(timeoutMessage(mode));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
