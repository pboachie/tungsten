# Security policy

## Supported versions

tungsten is pre-1.0. Only the latest commit on `main` receives security fixes.
Once releases are tagged, this table will list the supported release lines.

| Version | Supported |
|---|---|
| `main` (latest) | yes |
| anything older | no |

## Reporting a vulnerability

Please do not open a public issue for a security problem.

Use GitHub's private vulnerability reporting: open the **Security** tab of this
repository and choose **Report a vulnerability** (a GitHub Security Advisory
draft that only the maintainers can see). If that option is not available to
you, contact the maintainer [@pboachie](https://github.com/pboachie) through
GitHub and ask for a private channel, without describing the problem in public.

Please include:

- the affected component (compiler, a runtime, the generated CLI, the MCP
  server, the mock server);
- the commit or version, and how you built it;
- steps or an OpenAPI document that reproduce the problem, and its impact.

## What to expect

These are targets for a small project, not guarantees:

| Step | Target |
|---|---|
| Acknowledgement of your report | within 3 business days |
| Initial assessment and severity | within 7 days |
| Fix or mitigation for confirmed high-severity issues | within 30 days |
| Public advisory | after a fix is available, coordinated with you |

We will credit you in the advisory unless you prefer to stay anonymous.

## Scope

In scope: the compiler (`crates/`), the runtime libraries (`runtimes/`), and
code that tungsten generates when the flaw comes from tungsten's emitters (for
example a generated client that sends credentials to the wrong host or an MCP
server that bypasses its sandbox or confirmation rules).

Out of scope: vulnerabilities in the API described by an OpenAPI document,
issues in third-party dependencies that are already tracked upstream (report
those upstream; tell us if tungsten is exposed in a way that needs a patch),
and denial of service from inputs that are documented as unsupported.
