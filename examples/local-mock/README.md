# Develop against a local mock

Run tungsten mock and call it through the generated Python client with strict response validation.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; Python venv and pip.

From the repository root:

```sh
python3 examples/run.py --only local-mock
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: the generated client receives a nonempty typed Pet list from a loopback mock.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.
