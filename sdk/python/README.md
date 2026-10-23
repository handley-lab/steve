# Steve Python SDK

Build Python applications that start Steve threads, run turns, stream progress,
and control workspace access.

## Install

The distribution name is `steve-sdk`; registry publication is pending.
Use this checkout for local development.

The uv development environment resolves `steve-cli-bin` from the adjacent
runtime checkout at its development version. Published SDK metadata retains
the release runtime pin; release staging sets that pin to the chosen runtime
release. The runtime checkout contains no native payload until package staging.
Use `SteveConfig(steve_bin="/path/to/steve")` with a local CLI build.

## Quickstart

The SDK reuses your existing Steve authentication when one is already
available:

```python
from steve_sdk import Steve

with Steve() as codex:
    thread = codex.thread_start()
    result = thread.run("Explain this repository in three bullets.")
    print(result.final_response)
```

`thread.run(...)` returns a `TurnResult` containing the final response,
collected items, and token usage.

## Authentication

Configure the provider endpoint and supply credentials. Existing supplied
credentials are reused automatically. To store an API key:

```python
with Steve() as codex:
    codex.login_api_key("sk-...")
```

## Built-In Help

Use Python's standard `help(steve_sdk)`, `help(Steve)`, or
`python -m pydoc steve_sdk` documentation tools.

## Documentation

- [Getting started](docs/getting-started.md)
- [API reference](docs/api-reference.md)
- [Untrusted external messages](docs/api-reference.md#externalmessage)
- [FAQ](docs/faq.md)
- [Examples](examples/README.md)

The package is licensed under the
[repository Apache License 2.0](../../LICENSE).
