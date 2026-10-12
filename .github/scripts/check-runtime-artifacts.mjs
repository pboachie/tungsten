// SPDX-License-Identifier: AGPL-3.0-only
// Build and verify the exact npm tarballs consumers install. Never publishes.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { builtinModules, createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const output = resolve(process.argv[2] ?? mkdtempSync(join(tmpdir(), "tungsten-runtime-dist-")));
const packages = ["ts", "mcp", "webmcp"];
const names = ["@elysra/tungsten-runtime", "@elysra/tungsten-mcp", "@elysra/tungsten-webmcp"];
const workspaceVersion = readFileSync(join(root, "Cargo.toml"), "utf8").match(/^version = "([^"]+)"/m)?.[1];
assert(workspaceVersion, "workspace version is required");
mkdirSync(output, { recursive: true });
const scratch = mkdtempSync(join(tmpdir(), "tungsten-artifacts-"));
const run = (command, args, cwd, capture = false) => execFileSync(command, args, {
  cwd, encoding: "utf8", stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit",
});
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const tarballs = [];
try {
  // Build source snapshots: reusable test artifacts may be linked into the
  // checkout. Direct npm/tsc builds must never mutate those cached outputs.
  const buildRoot = join(scratch, "source", "runtimes");
  mkdirSync(buildRoot, { recursive: true });
  for (const pkg of packages) {
    const source = join(root, "runtimes", pkg);
    cpSync(source, join(buildRoot, pkg), {
      recursive: true,
      filter: path => !relative(source, path).split(/[\\/]/).some(part => ["node_modules", "dist", "target"].includes(part)),
    });
  }
  for (const [i, pkg] of packages.entries()) {
    const cwd = join(buildRoot, pkg);
    run(npm, ["ci", "--ignore-scripts"], cwd);
    run(npm, ["run", "build"], cwd);
    const [packed] = JSON.parse(run(npm, ["pack", "--json", "--pack-destination", output], cwd, true));
    const tgz = join(output, packed.filename);
    tarballs.push(tgz);
    const unpack = join(scratch, pkg);
    mkdirSync(unpack);
    run("tar", ["-xzf", tgz, "-C", unpack], root);
    const dir = join(unpack, "package");
    const manifest = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
    assert.equal(manifest.name, names[i]);
    assert.equal(manifest.version, workspaceVersion, `${manifest.name}: workspace version mismatch`);
    assert.equal(manifest.license, "Apache-2.0");
    assert.equal(readFileSync(join(dir, "LICENSE"), "utf8"), readFileSync(join(root, "runtimes/LICENSE"), "utf8"));
    assert.equal(readFileSync(join(dir, "README.md"), "utf8"), readFileSync(join(cwd, "README.md"), "utf8"), `${manifest.name}: package README must be included unchanged`);
    assert(readFileSync(join(dir, "README.md"), "utf8").startsWith(`# ${manifest.name}\n`), `${manifest.name}: package README heading mismatch`);
    assert.equal(manifest.repository.directory, `runtimes/${pkg}`);
    assert.equal(manifest.publishConfig.access, "public");
    assert.equal(manifest.type, "module");
    for (const field of ["dependencies", "peerDependencies", "optionalDependencies"]) {
      for (const [name, version] of Object.entries(manifest[field] ?? {})) {
        assert(!/^(?:file:|link:|workspace:|\.\.?\/|\/|git|https?:)/.test(version), `${manifest.name}: non-registry ${field} ${name}=${version}`);
      }
    }
    const targets = value => typeof value === "string" ? [value] : Object.values(value).flatMap(targets);
    assert(manifest.exports?.["."], `${manifest.name}: root export is required`);
    for (const target of targets(manifest.exports)) {
      assert(target.startsWith("./dist/"), `${manifest.name}: export outside dist: ${target}`);
      assert(existsSync(join(dir, target)), `${manifest.name}: missing export ${target}`);
    }
    assert(packed.files.some(file => file.path === "dist/index.d.ts"), "type declarations are packed");
    assert(packed.files.every(file => file.path === "package.json" || file.path === "LICENSE" || file.path === "README.md" || file.path.startsWith("dist/")), `${manifest.name}: unexpected packed files`);
  }
  const consumer = join(scratch, "consumer");
  mkdirSync(consumer);
  writeFileSync(join(consumer, "package.json"), JSON.stringify({ private: true, type: "module" }));
  // All runtime peers are supplied as archives in the same install; npm must
  // not resolve unpublished @elysra runtime packages from the registry or source links.
  run(npm, ["install", "--ignore-scripts", "--omit=dev", "--no-audit", "--no-fund", ...tarballs], consumer);
  run(npm, ["ls", "--omit=dev", ...names], consumer);
  for (const name of names) {
    const installed = join(consumer, "node_modules", name);
    assert(!lstatSync(installed).isSymbolicLink(), `${name}: consumer must install an archive, not a source link`);
    assert(realpathSync(installed).startsWith(consumer), `${name}: source tree leak`);
  }
  writeFileSync(join(consumer, "smoke.mjs"), `
    import assert from "node:assert/strict";
    import { ClientCore, RUNTIME_VERSION } from "@elysra/tungsten-runtime";
    import * as nodeAdapter from "@elysra/tungsten-runtime/node";
    import { createTungstenMcpServer } from "@elysra/tungsten-mcp";
    import { registerTungstenWebMcp, detectWebMcpBackend } from "@elysra/tungsten-webmcp";
    assert.equal(RUNTIME_VERSION, ${JSON.stringify(workspaceVersion)});
    for (const value of [ClientCore, createTungstenMcpServer, registerTungstenWebMcp, detectWebMcpBackend]) assert.equal(typeof value, "function");
    assert(Object.keys(nodeAdapter).length > 0);
  `);
  run(process.execPath, ["smoke.mjs"], consumer);

  // Parse the distributed JS, not source. Only the WebMCP root graph is
  // browser-safe; the explicitly separate runtime/node export is allowed.
  const ts = createRequire(join(root, "runtimes/mcp/package.json"))("typescript");
  const builtins = new Set(builtinModules.map(name => name.replace(/^node:/, "")));
  const visited = new Set();
  const walk = file => {
    file = resolve(file);
    if (visited.has(file)) return;
    visited.add(file);
    assert(existsSync(file), `browser graph missing ${file}`);
    const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
    const follow = spec => {
      assert(!spec.startsWith("node:") && !builtins.has(spec), `Node builtin in browser graph: ${spec} from ${file}`);
      assert(!spec.includes("@elysra/tungsten-runtime/node") && !spec.startsWith("@elysra/tungsten-mcp"), `Node adapter in browser graph: ${spec}`);
      if (spec.startsWith(".")) {
        const target = resolve(dirname(file), spec);
        assert(!relative(consumer, target).startsWith("..") && !isAbsolute(relative(consumer, target)), "browser graph escapes consumer");
        walk(target);
      } else {
        assert.equal(spec, "@elysra/tungsten-runtime", `unexpected browser dependency: ${spec}`);
        walk(join(consumer, "node_modules/@elysra/tungsten-runtime/dist/index.js"));
      }
    };
    const visit = node => {
      if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier) follow(node.moduleSpecifier.text);
      if (ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword || node.expression.getText(source) === "require")) {
        assert(ts.isStringLiteral(node.arguments[0]), `computed import in browser graph: ${file}`);
        follow(node.arguments[0].text);
      }
      ts.forEachChild(node, visit);
    };
    visit(source);
  };
  walk(join(consumer, "node_modules/@elysra/tungsten-runtime/dist/index.js"));
  walk(join(consumer, "node_modules/@elysra/tungsten-webmcp/dist/index.js"));
  const checksums = tarballs.map(file => `${createHash("sha256").update(readFileSync(file)).digest("hex")}  ${relative(output, file)}`).join("\n") + "\n";
  writeFileSync(join(output, "SHA256SUMS"), checksums);
  console.log(`Validated ${tarballs.length} npm archives, isolated imports and ${visited.size} browser modules; artifacts: ${output}`);
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
