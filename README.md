# Steve

Steve is a local coding agent forked from [OpenAI Codex](https://github.com/openai/codex).
It retains the upstream terminal UI, tools and harness while separating its
public command and automatic state discovery from Codex. Internal Rust crate
names remain upstream-compatible.

## Development status

Steve installers, npm packages and Python packages are not yet published.
Build from this checkout for local testing; do not use upstream Codex installers
to install Steve. Native Windows/macOS packaging remains unverified. The linked
network inventory describes the original pinned baseline, not this branch.

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

The recommended Cambridge configuration uses Kimi for coding, blocking Guardian
review and baseline risk classification, with D3 alongside for decision comparison.
Put this in `~/.steve/config.toml`:

```toml
model = "moonshotai/Kimi-K3"
model_provider = "cambridge"
web_search = "disabled"
check_for_update_on_startup = false
approval_policy = "on-request"
approvals_reviewer = "auto_review"

[auto_review]
model_provider = "cambridge"
model = "moonshotai/Kimi-K3"

[features]
guardian_approval = true
guardianv2_decisions_comparison = true

[features.guardianv2]
enabled = true
model_provider = "cambridge"
model = "moonshotai/Kimi-K3"
comparison_model_provider = "cambridge"
comparison_model = "vllm-sr/d3"

[model_providers.cambridge]
name = "Cambridge HPC"
default_model = "moonshotai/Kimi-K3"
base_url = "https://llm.hpc.cam.ac.uk/v1"
env_key = "CAMBRIDGE_LLM_API_KEY"
wire_api = "responses"

[analytics]
enabled = false

[feedback]
enabled = false
```

Supply `CAMBRIDGE_LLM_API_KEY` in the process environment. Keep credentials out
of this repository. D3 uses the configured provider's `/decisions` endpoint;
its comparison result does not approve actions or replace the baseline classifier.
The tests establish protocol compatibility, not comparative safety quality.

OpenAI account login and managed account-token renewal are
removed; supplied API keys and configured authentication commands remain available.
Every inference provider needs an explicit `base_url`, including OpenAI providers.
Set `model_provider` and either `model` or that provider's `default_model`.
Catalogue ordering never selects a model.

Model discovery polls the explicitly configured providers at `models`, or their
full `model_catalog_url` when supplied. It accepts Codex-native catalogs and
OpenAI-compatible `data/id` lists. `steve models` and the app server's
`model/list` expose qualified choices such as `cambridge/moonshotai/Kimi-K3`,
explicit global/provider defaults, and provider discovery errors. Provider
defaults come from `default_model`, never from list order. Pass a qualified choice
with `-m` or a model-only API request. Explicit provider/model request pairs use
the native model ID; saved configuration retains that pair. Public thread model
selections, including settings acknowledgements, use the qualified choice.

`model_catalog_json` supplies metadata for live IDs, including context limits
and instructions missing from discovery. Metadata slugs must use the canonical
`provider/native-model-id` spelling. A local-only
entry does not make a model available. Metadata can mark an otherwise
unclassified model with `"non_chat": true`; advertised non-chat modes are excluded
from coding choices. `features.api_key_model_discovery = false` disables
discovery instead of advertising a static catalog as live.

The native provider transport uses the Responses API. A provider exposing another
protocol needs a compatible gateway; changing its URL or token alone does not
translate Anthropic Messages or Chat Completions requests.

Selecting Cambridge and disabling analytics/feedback does not prove that every
optional integration avoids OpenAI. See the [baseline network inventory](docs/steve-network-audit.md)
for destinations, outgoing data, controls and remaining verification.

## Guardian configuration

Automatic review can use a separate entry in `model_providers`:

```toml
approval_policy = "on-request"
approvals_reviewer = "auto_review"

[auto_review]
model_provider = "local_reviewer"
model = "your-reviewer-model"

[model_providers.local_reviewer]
name = "Local reviewer"
base_url = "http://localhost:8000/v1"
wire_api = "responses"
```

Set the reviewer model explicitly. A user-supplied local model catalogue may also
provide an `auto_review_model_override`; downloaded model metadata does
not choose a reviewer. The selected provider supplies model capabilities and prompt
metadata. Choosing a provider does not establish model or tool compatibility.

The asynchronous classifier has independent provider and model settings:

```toml
[features.guardianv2]
enabled = true
model_provider = "local_reviewer"
model = "your-classifier-model"
```

Its model must satisfy the classifier's output contract. The optional Decisions
comparison requires both `comparison_model_provider` and `comparison_model`; its
service uses the Decisions protocol rather than ordinary chat inference.

To disable Guardian completely, including reviewer/classifier prewarming, use:

```toml
[features]
guardian_approval = false
```

Use `approvals_reviewer = "user"` for manual approval. Managed settings requiring
Guardian review are incompatible with turning it off and fail visibly.

## Documentation and provenance

- [TypeScript SDK](sdk/typescript/README.md)
- [Python SDK](sdk/python/README.md)
- [Upstream Codex documentation](https://developers.openai.com/codex), including
  useful harness and MCP guidance; upstream command names may differ.

Steve retains the upstream [Apache-2.0 license](LICENSE) and [NOTICE](NOTICE).
