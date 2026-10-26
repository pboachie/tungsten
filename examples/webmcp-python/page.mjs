// SPDX-License-Identifier: AGPL-3.0-only
// The page side, run by the runner in Node: `node page.mjs <adapter> <app url>`.
// A stub `document.modelContext` stands in for a WebMCP browser, and `fetch`
// adds what a browser sends on its own: the session cookie and the Origin.

const [adapter, origin] = process.argv.slice(2);
const { registerRemoteWebMcp } = await import(adapter);
globalThis.location = new URL("/", origin);

function browser(user) {
  const tools = new Map();
  const backend = {
    kind: "document",
    modelContext: {
      async registerTool(tool, { signal }) {
        tools.set(tool.name, tool);
        signal.addEventListener("abort", () => tools.delete(tool.name));
      },
    },
  };
  const headers = { origin, ...(user ? { cookie: `demo_session=${user}-session` } : {}) };
  const send = (input, init) => fetch(input, { ...init, headers: { ...init.headers, ...headers } });
  const call = (name, args) => tools.get(name).execute(args, { signal: new AbortController().signal });
  return { tools, call, register: () => registerRemoteWebMcp({ endpoint: "/webmcp", backend, fetch: send }) };
}

function check(condition, what) {
  if (!condition) throw new Error(what);
}

const alice = browser("alice");
const registration = await alice.register();
console.log(`registered for alice: ${registration.registeredNames.join(", ")}`);

const listed = await alice.call("pets_pets_list", {});
check(listed.ok && Array.isArray(listed.value) && listed.value.length > 0, `list: ${JSON.stringify(listed)}`);
console.log("pets_pets_list: typed pets from the Python SDK");

const refused = await alice.call("pets_pets_delete", { pet_id: "pet-1" });
check(!refused.ok && refused.error.category === "CONFIRMATION_REQUIRED", `delete: ${JSON.stringify(refused)}`);
console.log("pets_pets_delete without a token: CONFIRMATION_REQUIRED, nothing sent");

const preview = await alice.call("pets_pets_delete__preview", { pet_id: "pet-1" });
const token = preview.value.confirmation_token;
check(typeof token === "string" && token, "preview token");

const bob = browser("bob");
const bobs = await bob.register();
const stolen = await bob.call("pets_pets_delete", { pet_id: "pet-1", confirmation_token: token });
check(!stolen.ok && stolen.error.category === "CONFIRMATION_REQUIRED", `bob: ${JSON.stringify(stolen)}`);
console.log("bob cannot use alice's confirmation token");

const done = await alice.call("pets_pets_delete", { pet_id: "pet-1", confirmation_token: token });
check(done.ok, `confirmed: ${JSON.stringify(done)}`);
console.log("alice's confirmed delete: done");

const anonymous = await browser(null).register().then(() => "registered", (error) => error.message);
check(anonymous.includes("401"), `anonymous: ${anonymous}`);
console.log("signed out: the tool listing is refused (401)");

await registration.dispose();
await bobs.dispose();
check(alice.tools.size === 0 && bob.tools.size === 0, "dispose");
console.log("dispose: every owned tool removed");
