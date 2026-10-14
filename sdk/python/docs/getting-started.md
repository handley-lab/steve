# Getting Started

This guide gets a local Steve Python SDK checkout running
with a multi-turn thread.

## 1. Install

Registry publication is pending. From `sdk/python`, prepare the local environment:

```bash
uv sync --group dev
```

Requirements:

- Python `>=3.10`
- An existing Steve account session, or one of the login flows below

The local runtime checkout contains no native payload. Build the CLI as described
in the [repository README](../../../README.md), then pass its absolute path in
`SteveConfig(steve_bin="/path/to/steve")` when constructing `Steve`:

```python
from steve_sdk import Steve, SteveConfig

with Steve(config=SteveConfig(steve_bin="/path/to/steve")) as codex:
    print(codex.models())
```

The remaining examples assume the same local CLI configuration.

## 2. Authenticate When Needed

Existing Steve authentication is reused automatically. For ChatGPT browser
login:

```python
from steve_sdk import Steve

with Steve() as codex:
    login = codex.login_chatgpt()
    print(login.auth_url)
    print(login.wait().success)
```

For device-code login:

```python
with Steve() as codex:
    login = codex.login_chatgpt_device_code()
    print(login.verification_url, login.user_code)
    print(login.wait().success)
```

For API-key login:

```python
with Steve() as codex:
    codex.login_api_key("sk-...")
    print(codex.account().account)
```

## 3. Run A Turn

```python
from steve_sdk import Steve, Sandbox

with Steve() as codex:
    thread = codex.thread_start(sandbox=Sandbox.workspace_write)
    result = thread.run("Say hello in one sentence.")

    print("Thread:", thread.id)
    print("Text:", result.final_response)
    print("Items:", len(result.items))
```

`Thread.run(...)` starts a turn, waits for completion, and returns
`TurnResult`. Plain strings are shorthand for `TextInput(...)`.

Use `Thread.turn(...)` when you need a `TurnHandle` for streaming, steering,
or interrupting an active turn.

For **untrusted content** from another agent, tool, or application, pass an
[`ExternalMessage`](api-reference.md#externalmessage). It retains tool-level
authority and does not establish user authorization or approval. Plain strings
and `TextInput` represent user input.

## 4. Choose Sandbox Access

Use one enum for the initial thread and later turn overrides:

```python
from steve_sdk import Steve, Sandbox

with Steve() as codex:
    thread = codex.thread_start(sandbox=Sandbox.workspace_write)
    thread.run("Make the requested changes.")
    review = thread.run("Review the diff only.", sandbox=Sandbox.read_only)
```

Available presets:

- `Sandbox.read_only`: read files without allowing writes.
- `Sandbox.workspace_write`: read files and write inside the workspace and
  configured writable roots; this is the normal default for workspace work.
- `Sandbox.full_access`: run without filesystem access restrictions.

When `sandbox=` is omitted, Steve uses its configured default. A turn override
also applies to subsequent turns on that thread.

## 5. Continue A Thread

```python
from steve_sdk import Steve

with Steve() as codex:
    thread = codex.thread_start()
    thread.run("Summarize Rust ownership in two bullets.")
    result = thread.run("Now explain it to a Python developer.")
    print(result.final_response)
```

To resume a stored thread later:

```python
with Steve() as codex:
    thread = codex.thread_resume("thr_123")
    print(thread.run("Continue where we left off.").final_response)
```

## 6. Use The Async Client

```python
import asyncio

from steve_sdk import AsyncSteve, Sandbox


async def main() -> None:
    async with AsyncSteve() as codex:
        thread = await codex.thread_start(sandbox=Sandbox.workspace_write)
        result = await thread.run("Continue where we left off.")
        print(result.final_response)


asyncio.run(main())
```

## 7. Get Help

Python's built-in documentation tools cover the curated SDK surface:

```python
import steve_sdk
from steve_sdk import Steve, SteveConfig

help(steve_sdk)
help(Steve)
help(SteveConfig)
```

```bash
python -m pydoc steve_sdk
```

## Developing From This Repository

Contributors working from a checkout can install development dependencies from
the repository:

```bash
cd sdk/python
uv sync --group dev
source .venv/bin/activate
```

## Next Stops

- [API reference](https://github.com/openai/codex/blob/main/sdk/python/docs/api-reference.md)
- [FAQ](https://github.com/openai/codex/blob/main/sdk/python/docs/faq.md)
- [Runnable examples](https://github.com/openai/codex/blob/main/sdk/python/examples/README.md)
