// SPDX-License-Identifier: AGPL-3.0-only
// Stream a file up and down through the generated TypeScript SDK.
// The runner copies this file next to the generated package and runs
// `node transfer.mjs BASE_URL`.
import { createHash } from "node:crypto";
import { createReadStream, createWriteStream, readFileSync, rmSync } from "node:fs";
import { Readable } from "node:stream";
import { finished } from "node:stream/promises";
import { UploadSource } from "@elysra/tungsten-runtime";
import { saveDownload } from "@elysra/tungsten-runtime/node";
import { FilesClient } from "./dist/index.js";

const SIZE = 16 * 1024 * 1024;
const base = process.argv[2];

// A 16 MiB input file, written in pieces.
const digest = createHash("sha256");
const out = createWriteStream("input.bin");
for (let i = 0; i < SIZE / 65536; i++) {
  const piece = Buffer.alloc(65536, i % 251);
  digest.update(piece);
  if (!out.write(piece)) await new Promise((resolve) => out.once("drain", resolve));
}
out.end();
await finished(out);
const sha256 = digest.digest("hex");

const client = new FilesClient({ baseUrl: base });
// The file, read again from the start for every attempt.
const source = UploadSource.recreatable(() => Readable.toWeb(createReadStream("input.bin")), { length: SIZE, sha256 });
const stored = await client.files.update({ name: "report.bin", body: source });
if (!stored.ok) throw new Error(`upload failed: ${JSON.stringify(stored.error)}`);
if (stored.value.sha256 !== sha256) throw new Error("the server stored different bytes");
console.log(`typescript: uploaded ${stored.value.bytes} bytes, sha256 matches`);

rmSync("copy.bin", { force: true });
const download = await client.files.getDownload({ name: "report.bin" });
if (!download.ok) throw new Error(`download failed: ${JSON.stringify(download.error)}`);
// The server suggests a name; the caller still chooses the destination.
if (download.value.filename !== "report.bin") throw new Error(`unexpected file name ${download.value.filename}`);
const saved = await saveDownload(download.value, "copy.bin");
if (!saved.ok) throw new Error(`save failed: ${JSON.stringify(saved.error)}`);
if (createHash("sha256").update(readFileSync("copy.bin")).digest("hex") !== sha256) throw new Error("the copy differs");
console.log(`typescript: downloaded ${saved.value.bytes} bytes to copy.bin, sha256 matches`);

const again = await client.files.getDownload({ name: "report.bin" });
const refused = again.ok ? await saveDownload(again.value, "copy.bin") : again;
console.log(`typescript: second save refused: ${refused.ok ? "no" : refused.error.code}`);

const capped = await client.files.getDownload({ name: "report.bin" }, { maxBytes: 1024 * 1024 });
console.log(`typescript: capped download refused: ${capped.ok ? "no" : capped.error.code}`);
const missing = await client.files.getDownload({ name: "missing.bin" });
console.log(`typescript: missing file: ${missing.ok ? "found" : missing.error.category}`);
