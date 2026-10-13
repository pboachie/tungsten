// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded stdio discovery and first call through the freshly generated MCP server.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(super) fn run(directory: &Path, base_url: &str, operation_id: &str) -> Result<(), String> {
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", CHECK, base_url, operation_id])
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "cannot start the MCP first-use check")?;
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err("MCP discovery, preview or local tool call failed".into()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("MCP first-use check timed out or could not be observed".into());
            }
        }
    }
}

const CHECK: &str = r#"
import {spawn} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
import {resolve} from 'node:path';
const [baseUrl, operationId] = process.argv.slice(1);
const base = new URL(baseUrl);
if (base.protocol !== 'http:' || !['127.0.0.1', '[::1]'].includes(base.hostname) || base.username || base.password) throw new Error('loopback mock required');
const manifest = JSON.parse(readFileSync('manifest.json', 'utf8'));
const tool = manifest.tools.find(t => t.kind === 'operation' && t.target === operationId);
if (!tool) throw new Error('operation is not exposed by MCP');
const source = readFileSync('dist/server.js', 'utf8');
const variable = name => {
  const match = source.match(new RegExp('const ' + name + ' = ("[^"\\r\\n]*");'));
  if (!match) throw new Error('generated environment binding missing');
  return JSON.parse(match[1]);
};
const env = {[variable('BASE_URL_ENV')]: baseUrl, [variable('MODE_ENV')]: 'discrete'};
if (process.env.SystemRoot) env.SystemRoot = process.env.SystemRoot;
const bootstrap = `
const base = ${JSON.stringify(base.origin)};
const original = globalThis.fetch;
globalThis.fetch = (input, options) => {
 const url = new URL(typeof input === 'string' || input instanceof URL ? input : input.url);
 const method = options?.method ?? (typeof input === 'object' ? input.method : undefined) ?? 'GET';
 if (url.origin !== base || url.username || url.password || method.toUpperCase() !== 'GET') throw new Error('non-local GET blocked');
 return original(input, {...options, redirect:'error'});
};
setTimeout(() => process.exit(1), 18000).unref();
await import(${JSON.stringify(pathToFileURL(resolve('dist/server.js')).href)});
`;
const server = spawn(process.execPath, ['--input-type=module', '-e', bootstrap], {env, stdio:['pipe','pipe','ignore'], windowsHide:true});
let sequence = 0, buffered = '', total = 0;
const pending = new Map();
const fail = error => { for (const entry of pending.values()) entry.reject(error); pending.clear(); };
server.on('error', fail);
server.on('exit', () => fail(new Error('MCP server exited')));
server.stdin.on('error', fail);
server.stdout.setEncoding('utf8');
server.stdout.on('data', data => {
 total += Buffer.byteLength(data, 'utf8');
 if (total > 8 * 1024 * 1024) { fail(new Error('MCP output limit')); server.kill(); return; }
 buffered += data;
 for (;;) {
  const end = buffered.indexOf('\n'); if (end < 0) break;
  const line = buffered.slice(0, end); buffered = buffered.slice(end + 1);
  try {
   const message = JSON.parse(line), entry = pending.get(message.id);
   if (entry) { pending.delete(message.id); message.error ? entry.reject(new Error('MCP protocol error')) : entry.resolve(message.result); }
  } catch { fail(new Error('invalid MCP response')); server.kill(); }
 }
});
const send = message => server.stdin.write(JSON.stringify({jsonrpc:'2.0', ...message}) + '\n');
const request = (method, params = {}) => new Promise((resolve, reject) => {
 const id = ++sequence; pending.set(id, {resolve,reject}); send({id,method,params});
});
const timer = setTimeout(() => { fail(new Error('MCP timeout')); server.kill(); }, 15000);
try {
 const initialized = await request('initialize', {protocolVersion:'2024-11-05', capabilities:{}, clientInfo:{name:'tungsten-first-use',version:'1'}});
 if (!initialized?.capabilities?.tools) throw new Error('MCP tools unavailable');
 send({method:'notifications/initialized'});
 let cursor, found = false;
 for (let page = 0; page < 100; page++) {
  const listed = await request('tools/list', cursor ? {cursor} : {});
  found ||= listed.tools?.some(t => t.name === tool.name);
  cursor = listed.nextCursor; if (!cursor) break;
 }
 if (!found || cursor) throw new Error('MCP discovery incomplete');
 const args = {};
 if (tool.safety !== 'read_only') {
  const preview = await request('tools/call', {name:'preview', arguments:{tool:tool.name, arguments:{}}});
  if (preview.isError) throw new Error('MCP preview failed');
  const token = preview.structuredContent?.confirmation_token;
  if (tool.reserved.confirmationToken) {
   if (typeof token !== 'string' || !token) throw new Error('MCP confirmation missing');
   args[tool.reserved.confirmationToken] = token;
  }
 }
 const result = await request('tools/call', {name:tool.name, arguments:args});
 if (result.isError || !Array.isArray(result.content)) throw new Error('MCP call failed');
} finally {
 clearTimeout(timer);
 server.stdin.end();
 server.kill();
 await new Promise(resolve => { if (server.exitCode !== null || server.signalCode !== null) resolve(); else server.once('exit', resolve); });
}
"#;
