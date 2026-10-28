// SPDX-License-Identifier: AGPL-3.0-only
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = path => readFileSync(path, "utf8");
const capture = (path, pattern) => {
  const value = read(path).match(pattern)?.[1];
  assert(value, `${path}: cannot find the declared version`);
  return value;
};
const tomlVersion = (path, section) => {
  const text = read(path).split(`[${section}]`)[1]?.split(/^\[/m)[0];
  assert(text, `${path}: missing ${section}`);
  const value = text.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  assert(value, `${path}: missing version`);
  return value;
};
const workspace = tomlVersion("Cargo.toml", "workspace.package");
const tag = process.env.CI_COMMIT_TAG || process.env.RELEASE_TAG || "";
assert(tag || process.argv.includes("--workspace"), "set RELEASE_TAG/CI_COMMIT_TAG, or pass --workspace for an untagged source check");
if (tag) {
  assert.match(tag, /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/, "invalid release tag");
  for (const part of tag.split("-").slice(1).join("-").split(".")) {
    assert(!/^0\d+$/.test(part), "numeric prerelease identifiers cannot have leading zeroes");
  }
}
const version = tag ? tag.slice(1) : workspace;
assert.equal(workspace, version, "release tag and workspace version differ");
const expect = (path, actual) => assert.equal(actual, version, `${path}: release version mismatch`);
for (const pkg of ["ts", "mcp", "webmcp"]) {
  const path = `runtimes/${pkg}/package.json`;
  const manifest = JSON.parse(read(path));
  expect(path, manifest.version);
  const lock = JSON.parse(read(`runtimes/${pkg}/package-lock.json`));
  expect(`${pkg} lock root`, lock.version);
  expect(`${pkg} lock package`, lock.packages[""].version);
}
expect("Python manifest", tomlVersion("runtimes/python/pyproject.toml", "project"));
expect("Python MCP manifest", tomlVersion("runtimes/mcp-python/pyproject.toml", "project"));
expect("Rust runtime manifest", tomlVersion("runtimes/rust/Cargo.toml", "workspace.package"));
expect("Java manifest", capture("runtimes/java/pom.xml", /<version>([^<]+)<\/version>/));
expect("C# manifest", capture("runtimes/csharp/Tungsten.Runtime.csproj", /<Version>([^<]+)<\/Version>/));
expect("F# manifest", capture("runtimes/fsharp/Tungsten.Runtime.FSharp.fsproj", /<Version>([^<]+)<\/Version>/));
expect("Kotlin manifest", capture("runtimes/kotlin/build.gradle.kts", /^version\s*=\s*"([^"]+)"/m));
expect("Kotlin Java runtime dependency", capture("runtimes/kotlin/build.gradle.kts", /api\("io\.github\.pboachie\.tungsten:tungsten-runtime:([^"]+)"\)/));
expect("Kotlin Maven manifest", capture("runtimes/kotlin/pom.xml", /<version>([^<]+)<\/version>/));
expect("Kotlin Maven Java runtime dependency", capture("runtimes/kotlin/pom.xml", /<artifactId>tungsten-runtime<\/artifactId>\s*<version>([^<]+)<\/version>/));
expect("Kotlin coroutines manifest", capture("runtimes/kotlin/coroutines/build.gradle.kts", /^version\s*=\s*"([^"]+)"/m));
expect("Kotlin coroutines Java runtime dependency", capture("runtimes/kotlin/coroutines/build.gradle.kts", /api\("io\.github\.pboachie\.tungsten:tungsten-runtime:([^"]+)"\)/));
expect("Kotlin coroutines Kotlin layer dependency", capture("runtimes/kotlin/coroutines/build.gradle.kts", /api\("io\.github\.pboachie\.tungsten:tungsten-runtime-kotlin:([^"]+)"\)/));
expect("Kotlin coroutines Maven manifest", capture("runtimes/kotlin/coroutines/pom.xml", /<version>([^<]+)<\/version>/));
expect("Kotlin coroutines Maven Kotlin layer dependency", capture("runtimes/kotlin/coroutines/pom.xml", /<artifactId>tungsten-runtime-kotlin<\/artifactId>\s*<version>([^<]+)<\/version>/));
expect("Kotlin coroutines Maven Java runtime dependency", capture("runtimes/kotlin/coroutines/pom.xml", /<artifactId>tungsten-runtime<\/artifactId>\s*<version>([^<]+)<\/version>/));
expect("Dart manifest", capture("runtimes/dart/pubspec.yaml", /^version:\s*(\S+)/m));
expect("Go emitter default", capture("emitters/go/main.go", /const defaultRuntimeVersion\s*=\s*"v([^"]+)"/));

// Identity headers must describe the same runtime as the package metadata.
for (const [path, pattern] of [
  ["runtimes/ts/src/version.ts", /RUNTIME_VERSION\s*=\s*"([^"]+)"/],
  ["runtimes/python/src/tungsten_runtime/_core.py", /RUNTIME_VERSION:\s*Final\s*=\s*"([^"]+)"/],
  ["runtimes/java/src/main/java/io/github/pboachie/tungsten/runtime/RuntimeIdentity.java", /VERSION\s*=\s*"([^"]+)"/],
  ["runtimes/csharp/src/ClientCore.cs", /Version\s*=\s*"([^"]+)"/],
  ["runtimes/fsharp/src/Identity.fs", /let Version\s*=\s*"([^"]+)"/],
  ["runtimes/kotlin/src/main/kotlin/io/github/pboachie/tungsten/runtime/kotlin/KotlinRuntime.kt", /VERSION:\s*String\s*=\s*"([^"]+)"/],
  ["runtimes/dart/lib/tungsten_runtime.dart", /runtimeVersion\s*=\s*'([^']+)'/],
  ["runtimes/go/client.go", /const Version\s*=\s*"([^"]+)"/],
  ["runtimes/ruby/lib/tungsten/runtime/version.rb", /VERSION\s*=\s*"([^"]+)"/],
]) expect(path, capture(path, pattern));
console.log(`Release manifests and runtime identities match ${tag || `workspace ${version}`}`);
