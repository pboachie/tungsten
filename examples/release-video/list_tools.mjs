// SPDX-License-Identifier: AGPL-3.0-only
// List the real generated MCP catalog locally. Never invoke any API operation.
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const project=path.resolve(process.argv[2]);
const packageRoot=path.join(project,'generated/mcp');
const sdk=path.join(packageRoot,'node_modules/@modelcontextprotocol/sdk/dist/esm');
const { Client }=await import(pathToFileURL(path.join(sdk,'client/index.js')));
const { StdioClientTransport }=await import(pathToFileURL(path.join(sdk,'client/stdio.js')));
const client=new Client({name:'public-zrotext-demo',version:'1.0.0'});
const transport=new StdioClientTransport({
  command:process.execPath,args:[path.join(packageRoot,'dist/server.js')],
  env:{PATH:process.env.PATH,SystemRoot:process.env.SystemRoot},stderr:'pipe',
});
await client.connect(transport);
try {
  const {tools}=await client.listTools();
  console.log(`Generated MCP catalog: ${tools.length} tools`);
  for(const tool of tools.filter(t=>t.annotations?.readOnlyHint===true).slice(0,4)) console.log('  '+tool.name+' [read-only]');
  console.log('Local tools/list only; no API operation invoked.');
} finally { await client.close(); }
