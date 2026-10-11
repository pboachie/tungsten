// SPDX-License-Identifier: AGPL-3.0-only
import assert from "node:assert/strict";
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
  const match = url.pathname.match(new RegExp(`^/repos/${repoId}/pipeline/(\\d+)$`));
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

async function main() {
  if (process.env.WOODPECKER_RELEASE !== "true") {
    console.log("Woodpecker gate is disabled; using GitHub artifact validation.");
    return;
  }
  const sha = process.env.GITHUB_SHA;
  const tag = process.env.RELEASE_TAG;
  assert.match(sha ?? "", /^[0-9a-f]{40}$/);
  assert.match(tag ?? "", /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/);
  const deadline = Date.now() + 15 * 60_000;
  while (Date.now() < deadline) {
    const response = await fetch(`https://api.github.com/repos/pboachie/tungsten/commits/${sha}/statuses?per_page=100`, {
      headers: { Authorization: `Bearer ${process.env.GITHUB_TOKEN}`, Accept: "application/vnd.github+json" },
      signal: AbortSignal.timeout(30_000),
    });
    assert(response.ok, `GitHub status lookup failed: ${response.status}`);
    const expected = releaseStatus(await response.json(), sha, tag,
      process.env.WOODPECKER_REPO_ID ?? "", process.env.WOODPECKER_URL ?? "");
    if (expected) {
      const result = await fetch(expected.url, { signal: AbortSignal.timeout(30_000) });
      assert(result.ok, `Woodpecker pipeline lookup failed: ${result.status}`);
      verifyPipeline(await result.json(), expected);
      console.log(`Woodpecker validated ${tag} at ${sha}`);
      return;
    }
    console.log("Waiting for the Woodpecker npm release validation...");
    await setTimeout(15_000);
  }
  throw new Error("Woodpecker validation timed out; no npm publication is permitted");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
