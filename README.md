# Steve

Steve is a local coding agent forked from [OpenAI Codex](https://github.com/openai/codex).
It retains the upstream terminal UI, tools and harness while separating its
public command and automatic state discovery from Codex. Internal Rust crate
names remain upstream-compatible.

## Development status

Steve installers, npm packages and Python packages are not yet published.
Build from this checkout for local testing; do not use upstream Codex installers
to install Steve. Native Windows/macOS packaging and the complete OpenAI network
audit remain unfinished.

From `codex-rs`, build the CLI and local code-mode host:

```sh
CARGO_BUILD_JOBS=8 cargo build --locked \
  -p codex-cli --bin steve \
  -p codex-code-mode-host --bin steve-code-mode-host
./target/debug/steve
```

Build prerequisites and native dependencies remain those of the upstream
source tree. Package assembly additionally builds the platform sandbox helpers.

## Separate configuration

Steve defaults to `~/.steve`; `STEVE_HOME` selects another home. Project
configuration is discovered under `.steve`. It does not automatically import
Codex's home or project configuration.

For Cambridge HPC, put this provider configuration in `~/.steve/config.toml`:

```toml
model = "moonshotai/Kimi-K3"
model_provider = "cambridge"
web_search = "disabled"
check_for_update_on_startup = false

[model_providers.cambridge]
name = "Cambridge HPC"
base_url = "https://llm.hpc.cam.ac.uk/v1"
env_key = "CAMBRIDGE_LLM_API_KEY"
wire_api = "responses"

[analytics]
enabled = false

[feedback]
enabled = false
```

Supply `CAMBRIDGE_LLM_API_KEY` in the process environment. Keep credentials out
of this repository. No first-party login is needed for this provider.

Selecting Cambridge and disabling analytics/feedback does not prove that every
optional integration avoids OpenAI. See the [partial network audit](docs/steve-network-audit.md)
for destinations, outgoing data, controls and remaining verification.

## Documentation and provenance

- [TypeScript SDK](sdk/typescript/README.md)
- [Python SDK](sdk/python/README.md)
- [Upstream Codex documentation](https://developers.openai.com/codex), including
  useful harness and MCP guidance; upstream command names may differ.

Steve retains the upstream [Apache-2.0 license](LICENSE) and [NOTICE](NOTICE).
