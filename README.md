# Steve

Steve is a local coding agent forked from [OpenAI Codex](https://github.com/openai/codex).
It retains the upstream terminal UI, tools and harness while separating its
public command and automatic state discovery from Codex. Internal Rust crate
names remain upstream-compatible.

## Development status

Native packages are available as GitHub Actions artifacts for Linux, macOS and
Windows on x86-64 and ARM64. See [installing a binary package](docs/steve-install.md)
for downloads, checksum verification and installation. Installers, npm packages
and Python packages are not yet published; upstream Codex installers do not
install Steve. A source build of main commit `cee8158374` was traced using the
Cambridge configuration below: inference contacted `llm.hpc.cam.ac.uk`, with no
OpenAI destination observed. The [network inventory](docs/steve-network-audit.md)
records the scenarios and limits; release update fetching and optional paths
were not exercised. A passing full test baseline is not yet established.

To build from source, run this from the repository root. It creates a local
package containing the CLI, code-mode
host, executor, and platform sandbox helpers:

```sh
STEVE_REPO_ROOT="$PWD" CARGO_BUILD_JOBS=8 \
  python3 scripts/build_codex_package.py \
  --target x86_64-unknown-linux-gnu --package-dir dist/steve
./dist/steve/bin/steve
```

The example targets x86-64 Linux with glibc, including Ubuntu. On another
platform, pass its Rust target triple (see `--help`). Install Rust through
rustup, Python 3.11 or newer, and the native build dependencies first; Ubuntu needs `build-essential`,
`pkg-config`, `libcap-dev`, and `libasound2-dev`.

The code-mode host requires sandbox-enabled V8. Plain Cargo builds try Deno's
default archive location, which does not provide the required sandbox pair.
The package builder instead downloads the version-pinned archive and matching
Rust bindings from `openai/codex` releases, verifies their checksum manifest
against this checkout, then verifies both files. These are build inputs, not
an inference service. This remains an OpenAI-published binary dependency.
To build V8 from source instead, set `V8_FROM_SOURCE=1`; that uses rusty_v8's
separate native toolchain and source-download path and takes substantially more
time and disk. See [V8 build provenance](third_party/v8/README.md).

The `Steve binary build` workflow runs after updates to `main` and can also be
started manually for an exact candidate commit. Each job checks the linked V8
sandbox and extracted package, then uploads an archive, SHA-256 checksum and
build provenance. All six platforms passed for candidate `c5ba040781` in the
[initial successful package run](https://github.com/handley-lab/steve/actions/runs/41686805704).
These checks cover package construction and startup, not a complete interactive
session or the full test suite. Packages are not release-signed or notarized.

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
