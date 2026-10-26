# Reuse one configuration profile across projects

Two projects share reviewed defaults through local configuration profiles and
override what they need in their own `tungsten.yml`:

| Path | Role |
|---|---|
| `profiles/org.yml` | Organization defaults: package versions and the minimum Python. |
| `profiles/library.yml` | Library defaults; includes `org.yml` and adds dataclass models. |
| `storefront/` | An ordinary app with one TypeScript SDK and an explicit version override. |
| `catalog/` | A multi-target library (TypeScript, Python, Rust) with its own package names. |

Precedence is explicit and deterministic. Included profiles apply first, then the
including profile, then each selected profile in manifest order. Explicit fields
in the project's `targets` always win. Profiles only provide defaults for targets
the project declares: `storefront` gets no Python or Rust SDK from `org.yml`.
Profiles cannot set output directories, runtime paths, the Python module or the
Rust CLI, and they are never downloaded.

## Run it

Build the matching compiler, then run the check from the repository root:

```sh
cargo build -p tungsten-cli
python3 examples/profile-reuse/check.py
```

The check copies this directory to a temporary location, explains each project's
effective configuration with `tungsten --json config`, generates both projects
and verifies the resulting package metadata. It installs nothing and makes no
network requests. Set `TUNGSTEN` to use a different compiler binary.

To see where every value comes from in one project:

```sh
cd examples/profile-reuse/catalog
tungsten config
```

Each supported option lists its winning source (`project` or `profile[N]`) and
the earlier declarations it shadowed. Sensitive values and physical paths are
omitted.

## Start a new project from a profile

`tungsten init` selects the same profiles. It composes them before it writes any
file, and refuses invalid or remote profiles:

```sh
tungsten init --no-interactive --dir examples/profile-reuse/new-app \
  --from ../openapi.json --targets typescript,python \
  --profile ../profiles/library.yml
```

The guided `tungsten init` setup asks the same question in its advanced section.
