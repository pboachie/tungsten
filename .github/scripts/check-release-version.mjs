// SPDX-License-Identifier: AGPL-3.0-only
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const tag = process.env.CI_COMMIT_TAG ?? process.env.RELEASE_TAG ?? "";
assert.match(tag, /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/, "invalid release tag");
const version = tag.slice(1);
const workspace = readFileSync("Cargo.toml", "utf8").match(/^version = "([^"]+)"/m)?.[1];
assert.equal(workspace, version, "release tag and workspace version differ");
for (const pkg of ["ts", "mcp", "webmcp"]) {
  const manifest = JSON.parse(readFileSync(`runtimes/${pkg}/package.json`, "utf8"));
  assert.equal(manifest.version, version, `${pkg}: release version mismatch`);
}
console.log(`Release manifests match ${tag}`);
