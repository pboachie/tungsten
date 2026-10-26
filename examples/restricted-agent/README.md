# Restricted agent integration

An MCP server on the TypeScript SDK for a help-desk agent that may read
tickets, must preview and confirm before opening one, and is never offered
deletion. Reviewed transport defaults come from a shared profile; the project
adds only what is specific to it.

| Path | Role |
|---|---|
| `profiles/agent.yml` | Shared agent defaults: proxy, `NO_PROXY` and CA bundle references, the credential provider and the SDK version. |
| `tungsten.yml` | The project: TypeScript and MCP targets, a fixed server, and its own account and environment names. |
| `agent.yml` | The restrictions: `createTicket` is irreversible (local preview and an argument-bound confirmation token), `deleteTicket` is hidden from agents. |

The profile holds references only (`{env: CORP_HTTPS_PROXY}` and so on). tungsten
never reads the variables, and certificate verification cannot be turned off by
configuration. Profiles cannot set agent tools, approvals or inputs, so the
restrictions stay in this project's reviewed `agent.yml`. Hidden operations stay in
the SDK for code that calls them deliberately.

## Run it

Build the matching compiler, then run the check from the repository root:

```sh
cargo build -p tungsten-cli
python3 examples/restricted-agent/check.py
```

The check copies this directory to a temporary location, explains the effective
configuration with `tungsten --json config`, generates with `--strict` and verifies
the MCP tool manifest: three tools, a confirmation token on `createTicket`, and no
delete tool. It installs nothing and makes no network requests. Set `TUNGSTEN` to
use a different compiler binary.

To see each value's source:

```sh
cd examples/restricted-agent
tungsten config
```

## Start a similar project

```sh
tungsten init --no-interactive --preset agent --from openapi.json --dir my-agent \
  --hide-operation tickets.deleteTicket --require-confirmation tickets.createTicket
```

`--preset agent` configures the TypeScript SDK and MCP server and generates with
`--strict`; select a shared profile with `--profile`.
