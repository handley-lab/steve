# Steve network paths

This is a partial static audit of Steve's network paths. It describes
source-level destinations and controls, not a verified network allowlist.
The sections below include inference, model discovery, telemetry, feedback,
MCP, selected authentication flows, cloud-task submission, installers and assets.
Plugin runtime coverage and other cloud-task operations still need coverage.

## TUI update discovery

The release-build TUI checks the Steve GitHub release API, the Steve npm package
registry and, for Homebrew installations, the Steve cask API. These GETs send
client headers and download version/package metadata; the reviewed functions
do not attach conversation bodies. Their identifiers now target Steve, whose
publication availability has not been verified. The lookup is suppressed by
`check_for_update_on_startup=false` and source-build versions; this module is
also excluded from debug builds. Daemon installations skip this TUI lookup and
have their own update mechanism. A debug-build smoke test therefore cannot
verify release-build update behavior. Sources:
`codex-rs/tui/src/updates.rs` and `npm_registry.rs`.

## Remote plugin catalogue and account mutations

The reviewed remote plugin-service requests use `chatgpt_base_url` and require
first-party backend authentication. Catalogue GETs include `/ps/plugins/list`,
`/ps/plugins/workspace/shared`, `/ps/plugins/installed`, plugin-specific detail
paths and `/ps/plugins/suggested/codex`. Query parameters identify scope,
collection, pagination and whether download URLs are requested. Authentication
and product SKU accompany these downloads.

Installing and uninstalling remote plugins POST to
`/ps/plugins/{plugin_id}/install` and `/uninstall`. Installation can include an
attempt ID and requests Apps requiring authentication. These operations change
remote account state; they are not just local package installation. The
catalogue can return bundle download URLs, whose acquisition and runtime
behavior are not established by this section. Local plugin interoperability
does not require mechanically renaming the external service paths. Avoid
first-party remote plugin operations and inspect installed plugin transports
when preventing OpenAI traffic. Analytics and feedback flags do not gate the
reviewed request functions. No plugin-service request was made for this audit.
Source: `codex-rs/core-plugins/src/remote.rs`.

### Plugin sharing uploads

Saving a remote plugin share archives the selected local plugin directory. It
POSTs filename, gzip MIME type, archive size and optional plugin ID to
`/public/plugins/workspace/upload-url`, then PUTs the archive bytes to the
returned upload URL. Finalization POSTs file ID, ETag, discoverability and share
targets to `/public/plugins/workspace` or its plugin-specific path. Sharing
therefore sends local plugin content to a service-selected storage endpoint;
it is distinct from catalogue downloads and inference. The reviewed save path
requires first-party backend authentication. Changing targets PUTs principals
and discoverability to `/ps/plugins/{id}/shares`; deletion sends an authenticated
DELETE to `/public/plugins/workspace/{id}`. Analytics and feedback consent are
not checked in these functions. Do not invoke these remote-sharing operations
when local content must not be uploaded. No sharing operation was exercised.
Source: `codex-rs/core-plugins/src/remote/share.rs`.

### Plugin bundle downloads

Remote bundles are GETs to service-supplied HTTPS URLs, including redirects.
The reviewed download function adds no explicit backend credential header or
conversation body. HTTPS scheme validation does not constrain the destination
to an OpenAI host: inspect the returned URL and the configured HTTP client's
headers/policy. Downloaded plugin content can subsequently define tools and
network transports; obtaining the bundle does not establish their runtime
behavior. No bundle-download request was exercised for this audit.
Source: `codex-rs/core-plugins/src/remote_bundle.rs`.

## Realtime voice

Realtime setup starts from the selected provider. WebSocket and WebRTC call
base URLs can be overridden separately by
`experimental_realtime_ws_base_url` and
`experimental_realtime_webrtc_call_base_url`; an existing call can also supply
its sideband base. These overrides need inspection even when ordinary inference
uses Cambridge. This source routing does not establish Cambridge support for
the realtime protocols.

WebRTC creation POSTs SDP and session configuration to `realtime/calls`
(or `live` for the reviewed frameless API shape). Backend requests use JSON;
the other reviewed session shape uses multipart SDP and session JSON. Realtime
WebSocket messages include conversation text and encoded audio frames. These
are outgoing inference inputs, with response audio/text coming back, rather
than diagnostic uploads or asset downloads. Authentication is attached by the
API client. No voice session was exercised for this audit. Sources:
`codex-rs/core/src/realtime_conversation.rs`, `client.rs`,
`codex-rs/codex-api/src/endpoint/realtime_call.rs` and
`realtime_websocket/methods.rs`, `methods_v1.rs`.

## Hosted Apps and connector discovery

Connector directory requests use `chatgpt_base_url` and first-party backend
authentication. They GET `/connectors/directory/list` (with pagination tokens
and `external_logos=true`) and, for workspace accounts, `list_workspace`.
These download catalogue metadata but send authentication, client headers and
product SKU. Metadata lookup POSTs requested app IDs and `include_tools` to
`/ps/apps/batch`; its response supplies app metadata and optional tool summaries.
Neither operation uses the Cambridge inference base.

The hosted Apps/plugin MCP constructor targets `/ps/mcp` beneath the ChatGPT
backend base (or `/api/codex` for other bases). It configures ChatGPT
authentication, product SKU and optional originator headers; the optional
`STEVE_CONNECTORS_TOKEN` selects a bearer-token environment input. Calls through
this transport send MCP tool arguments and metadata, rather than merely fetching
documentation. The general MCP metadata section below applies.

The reviewed directory and host-owned Apps paths require the Apps feature and
backend authentication; `features.apps=false` disables those paths. Apps is
enabled by default, so changing only the inference provider is insufficient
when first-party credentials remain available. This does not prove that every
explicitly configured or plugin-registered MCP endpoint is suppressed. Inspect
the effective catalogue and the selected plugin transports as well. No hosted
Apps request was made for this audit. Sources:
`codex-rs/chatgpt/src/connectors.rs`, `chatgpt_client.rs`,
`codex-rs/connectors/src/lib.rs`, `codex-rs/codex-mcp/src/mcp/mod.rs` and
`codex-rs/features/src/lib.rs`.

## Remote control

The remote-control relay uses `chatgpt_base_url`, independently of the inference
provider. URL normalization adds `wham/remote/control/server/enroll`,
`server/refresh`, `server/pair` and `server/pair/status`; its WebSocket uses
`wham/remote/control/server`. Accepted remote hosts belong to ChatGPT or its
staging service; loopback hosts are also supported.

Enrollment posts the server name (derived from hostname), OS, architecture,
app-server version and installation ID, with authentication headers. Refresh
posts server and installation IDs. Pairing exchanges codes. The WebSocket
serializes app-server outgoing messages, including notifications, into relay
envelopes with client/stream/sequence identifiers. This is a transport for
session data and control, not a documentation download; the possible payload
content depends on which app-server messages are sent.

Remote control has its own desired state, authentication and persisted pairing
state. Daemon settings default to disabled. Startup can resolve persisted state,
so absence of an enable flag on one command does not establish that it is
inactive. Keep it disabled when OpenAI relay traffic is unwanted; analytics
and feedback consent are separate controls. No relay connection was exercised
for this audit. Sources:
`codex-rs/app-server-transport/src/transport/remote_control/protocol.rs`,
`server_api.rs`, `controller.rs`, `websocket.rs`,
`codex-rs/app-server/src/lib.rs` and
`codex-rs/app-server-daemon/src/settings.rs`.

Changing the inference provider does not change every service endpoint.
Disabling analytics and feedback does not disable MCP requests or explicitly
configured OpenTelemetry collectors.

## Observed Cambridge startup traffic

An instrumented debug-package run with a fresh Steve home, Cambridge Kimi,
Apps disabled, no configured MCP servers, analytics/feedback disabled and
startup update checks disabled completed a model turn. Tracing the process tree
showed TLS connections with SNI for Cambridge, ChatGPT and GitHub. The Git
subprocess explicitly fetched `https://github.com/openai/plugins.git`.
Changing only `features.plugins` to false in a second fresh home still completed
the Kimi turn; the trace then contained only Cambridge port-443 connections.
DNS and local Unix-socket activity also occurred. This establishes the observed
plugin-related startup traffic for these runs, not every possible network path
or the contents of encrypted ChatGPT requests.

Featured-plugin lookup GETs
`{chatgpt_base_url}/plugins/featured?platform=...`, optionally attaching backend
credentials when available. Steve does not fetch
`https://github.com/openai/plugins.git`, the GitHub archive of that repository,
or `https://chatgpt.com/backend-api/plugins/export/curated`. A curated snapshot
already stored under the Steve home is still read locally. User-configured Git
marketplaces remain a separate client. Plugins are enabled by default.
Disabling the feature stops the featured-plugin request and local plugin
loading together.

Raw local evidence: `~/.cache/codex-cambridge-build/steve-network-smoke-n05wyupc/`
and `steve-network-smoke-cey30wqi/` contain configuration, structured model
output, exit status and syscall traces. The tested archive predates later
daemon diagnostic wording changes. Sources:
`codex-rs/core-plugins/src/remote_legacy.rs`, `startup_sync.rs` and `manager.rs`.

## Cloud-task submission

Cloud-task initialization independently defaults to
`https://chatgpt.com/backend-api`, with `STEVE_CLOUD_TASKS_BASE_URL` as its
override. Cambridge inference configuration does not select this backend.
Environment discovery sends GitHub repository owner/name in request paths and
uses configured authentication headers. Task creation sends the user prompt,
environment ID, branch, QA setting and optional best-of-N metadata. A nonempty
`STEVE_STARTING_DIFF` adds the patch itself to the request body. These are
outbound work submissions, rather than documentation downloads. This section
does not establish every entry point or audit other cloud-task operations.
Sources: `codex-rs/cloud-tasks/src/lib.rs`,
`codex-rs/cloud-tasks/src/env_detect.rs` and
`codex-rs/cloud-tasks-client/src/http.rs`.

| Mechanism | Destination | Data leaving the client | Control |
| --- | --- | --- | --- |
| Inference | Selected provider's `base_url`; defaults can be `api.openai.com/v1` or `chatgpt.com/backend-api/codex` | Prompts, conversation context, tool definitions and results, model/reasoning settings, request metadata and authentication | Configure the intended provider and an explicit base URL. Provider credentials and first-party authentication have separate selection rules. |
| Model discovery | Provider `/models`, explicit catalog URL, or eligible first-party catalog fallback | Authentication, configured headers/query parameters and client version; receives model definitions and ETag | An explicit provider base or catalog URL prevents the first-party default-base substitution. Cache/refresh policy also controls fetching. |
| Built-in Statsig metrics | `https://ab.chatgpt.com/otlp/v1/metrics` | Instrument-specific usage and diagnostic measurements/tags and service resources | `[analytics].enabled = false` disables this built-in exporter. Debug builds also resolve Statsig to disabled; release builds otherwise enable its HTTP exporter. |
| Explicit OTLP | Configured HTTP/gRPC endpoints | Configured metrics, logs or traces and resource attributes; content depends on instrumentation and logging settings | Configure the individual exporters. Analytics consent does not override these explicitly configured collectors. |
| App-server analytics | `{chatgpt_base_url}/codex/analytics-events/events` | Event-specific IDs, activity/timing/model/token usage, command/file/tool activity, plugin/skill/MCP identifiers and errors, plus eligible authentication | `[analytics].enabled = false` disables this queue. Debug builds do not generally disable it. It is independent of the inference base URL. |
| Feedback | OpenAI project's Sentry DSN-derived envelope endpoint | User report and diagnostic/identity tags; requested attachments can include logs, rollouts, tool caches and history. Explicit extra files are handled separately from the include-logs flag. | `[feedback].enabled = false` blocks TUI feedback and app-server feedback upload. Low-level upload code relies on its caller to enforce consent. |
| OpenAI docs MCP | Configured `https://developers.openai.com/mcp`, attributed as `?source=codex` | Initialization identity/capabilities, tool names/arguments, configured authentication and MCP metadata; receives documentation and protocol results | This wrapper does not register the server. Leaving it unconfigured prevents this configured path. Analytics/feedback settings do not disable MCP metadata. |

A documentation request is still an outbound request. MCP metadata can include
call/thread/session IDs, model and reasoning settings, sandbox/review settings,
turn timing, workspace Git metadata and tracing context. Normal MCP metadata
removes some agent lineage and harness inventory fields; host-owned Apps have a
separate lineage path. A read-only tool is not a guarantee that no session
information leaves the client.

Feedback may enrich metadata from a rollout even when log attachments are not
requested. Explicit additional attachments must also be considered when
assessing a report's contents. The configured app-server feedback gate executes
before that upload work.

## Source evidence

Paths are relative to the repository root.

- Inference endpoint/auth selection: `codex-rs/model-provider-info/src/lib.rs`,
  `codex-rs/model-provider/src/auth.rs`, `codex-rs/codex-api/src/common.rs` and
  `codex-rs/codex-api/src/endpoint/responses.rs`.
- Catalog fallback and refresh: `codex-rs/model-provider/src/models_endpoint.rs`,
  `codex-rs/codex-api/src/endpoint/models.rs` and
  `codex-rs/models-manager/src/manager.rs`.
- Built-in versus explicit exporters: `codex-rs/core/src/otel_init.rs`,
  `codex-rs/otel/src/config.rs`, `codex-rs/otel/src/provider.rs` and
  `codex-rs/config/src/types.rs`.
- Analytics queue, authentication eligibility and payload schemas:
  `codex-rs/app-server/src/analytics_utils.rs`,
  `codex-rs/analytics/src/client.rs` and `codex-rs/analytics/src/events.rs`.
- Feedback upload and gates: `codex-rs/feedback/src/lib.rs`,
  `codex-rs/feedback/src/upload.rs`,
  `codex-rs/tui/src/chatwidget/slash_dispatch.rs` and
  `codex-rs/app-server/src/request_processors/feedback_processor.rs`.
- MCP attribution and metadata:
  `codex-rs/codex-mcp/src/openai_docs_source_attribution.rs`,
  `codex-rs/codex-mcp/src/rmcp_client.rs`,
  `codex-rs/rmcp-client/src/rmcp_client.rs`,
  `codex-rs/rmcp-client/src/trace_context.rs`,
  `codex-rs/core/src/mcp_tool_call.rs`, `codex-rs/core/src/turn_metadata.rs` and
  `codex-rs/core/src/responses_metadata.rs`.

A Cambridge profile with an explicit Cambridge base URL and disabled analytics
and feedback redirects ordinary inference/model discovery and disables built-in
analytics and feedback uploads. These settings do not disable plugin acquisition
or the other independently controlled paths described above. Effective
configuration, registered transports and observed requests must be checked for
the assembled executable; these settings do not prove absence of OpenAI traffic.

## Installer and external desktop paths

Steve's standalone daemon updater fetches its shell or PowerShell installer
from handley-lab/steve GitHub release assets. The TUI's standalone update action
uses the same authority. The shell installer requests Steve release metadata,
checksums and archives from GitHub; its former OpenAI mirror is removed. These
are download paths, with normal request/network metadata leaving the host.
This does not cover every update-discovery path or prove runtime behavior.
Sources: `codex-rs/app-server-daemon/src/update_loop.rs`,
`codex-rs/tui/src/update_action.rs` and `scripts/install/install.sh`.

The macOS desktop command deliberately operates the external Codex desktop
application. If absent, it downloads Codex.dmg or Codex-latest-x64.dmg from
`persistent.oaistatic.com/codex-app-prod` through curl. The download call adds
no conversation body or workspace argument. After signature verification,
it opens `codex://threads/new?path=...`, passing the workspace path to that
external local application. The desktop application's subsequent network
behavior is outside this source audit. Avoid invoking this command when
external desktop participation is unwanted; a Cambridge inference provider
does not change this integration. Source: `codex-rs/cli/src/desktop_app/mac.rs`.

## Guardian Decisions comparison

Guardian scoring does not build or send a request to
`https://api.openai.com/v1/decisions`. The feature key
`guardianv2_decisions_comparison` remains so older configuration files parse;
its stage is removed and no code reads it. Baseline Guardian classification
still uses the configured model provider. Source:
`codex-rs/features/src/lib.rs` and
`codex-rs/ext/guardian-v2/src/async_scorer/`.

## Built-in pet assets

Loading a selected built-in pet checks its versioned cache under STEVE_HOME.
If the spritesheet is absent or invalid, the TUI issues a GET to
`https://persistent.oaistatic.com/codex/pets/v1/<spritesheet filename>`.
The acquisition code adds no conversation body, tool evidence or explicit
authentication header; transport defaults and network metadata still apply.
The response is an image, validated for expected dimensions before caching.
A valid cache avoids this request. Custom pets bypass built-in acquisition
because their spritesheet source is local. Avoid selecting uncached built-in
pets when this OpenAI asset request is unwanted. Analytics/feedback consent
does not gate the download in this acquisition function. Sources:
`codex-rs/tui/src/pets/asset_pack.rs` and `codex-rs/tui/src/pets/mod.rs`.
No asset request was made during this static audit.

## Browser and device-code login

First-party browser login defaults to issuer `https://auth.openai.com`. Its
`/oauth/authorize` URL carries client ID, redirect URI, requested scopes, PKCE,
state, originator and optional allowed workspace IDs. The callback exchanges
the authorization code and PKCE material through `/oauth/token` and stores
returned credentials in Steve's auth store. These are credential exchanges,
not model inference; changing the inference base does not retarget this issuer.

Device login POSTs client ID to `/api/accounts/deviceauth/usercode`, then polls
`/api/accounts/deviceauth/token` with the device-auth ID and user code before
the token exchange. Avoid invoking first-party browser/device login when using
Cambridge-only authentication. Sources: `codex-rs/login/src/server.rs` and
`codex-rs/login/src/device_code_auth.rs`. No credential exchange was performed
during this source audit.

## Managed token refresh and revocation

Resolving managed ChatGPT auth can proactively refresh credentials when an
access token approaches expiry, or by the stored refresh age when expiry is
unavailable. Refresh sends the refresh token and OAuth client ID to
`https://auth.openai.com/oauth/token` by default. The response can replace ID,
access and refresh tokens. `STEVE_REFRESH_TOKEN_URL_OVERRIDE` selects another
endpoint; the inference provider base does not. Source:
`codex-rs/login/src/auth/manager.rs`.

Logout attempts revocation only for managed ChatGPT credentials with a usable
refresh or access token, preferring the refresh token. It POSTs the token, token
type hint and optional client ID to `https://auth.openai.com/oauth/revoke`.
An explicit `STEVE_REVOKE_TOKEN_URL_OVERRIDE` takes precedence; otherwise an
overridden refresh endpoint can supply its origin. Local auth removal still
proceeds if revocation fails. Source: `codex-rs/login/src/auth/revoke.rs`.
Avoid storing first-party credentials in Steve's isolated state when these
exchanges are unwanted. Analytics/feedback settings do not gate them.
No refresh or revocation request was performed during this static audit.

## Personal access token metadata

Loading a first-party personal access token performs an authenticated GET to
`https://auth.openai.com/api/accounts/v1/user-auth-credential/whoami` by default.
It sends that token in bearer authentication and receives email, user/account
IDs, plan and account attributes. `STEVE_AUTHAPI_BASE_URL` overrides this auth
service base; the inference provider URL does not. This is credential validation
and metadata acquisition, not conversation inference. Avoid supplying first-party
access-token credentials for Cambridge-only operation. Sources:
`codex-rs/login/src/auth/personal_access_token.rs` and
`codex-rs/login/src/auth/manager.rs`. No token was read or submitted during
this static audit.

## Workload identity

First-party workload authentication uses the external environment contract
`OPENAI_FEDERATION_RULE_ID`, `OPENAI_IDENTITY_TOKEN_FILE` and optional
`OPENAI_WORKLOAD_IDENTITY_CONTEXT`, with a policy permitting ChatGPT login.
The login adapter selects production `https://auth.openai.com/oauth/token`
or staging `https://auth.api.openai.org/oauth/token` from its first-party
ChatGPT environment. The exchange reads the assertion file and POSTs its
contents, federation-rule ID, JWT-bearer grant type and optional context.
It receives short-lived access credentials and account/user/plan/scope metadata;
cache expiry or rejected credentials can require another exchange.

This is an outbound identity assertion, not a model request. Cambridge's
inference base does not select the first-party token URL. Avoid enrolling
first-party workload credentials in the Steve process when this path is
unwanted. Sources: `codex-rs/login/src/auth/workload_identity.rs`,
`codex-rs/workload-identity/src/exchange.rs` and
`codex-rs/protocol/src/shell_environment.rs`. No assertion file was read and
no authentication request made during this source audit.

## Agent identity transport

First-party agent identity registers a public signing key, agent bill of
materials and capabilities through authenticated POST
`/api/accounts/v1/agent/register` on the selected OpenAI auth environment.
Task registration POSTs a timestamp and signed assertion to
`/api/accounts/v1/agent/<runtime ID>/task/register`, returning a task identity.
JWT verification can GET signing keys from the ChatGPT backend's
`/wham/agent-identities/jwks`. These are identity/provenance exchanges, separate
from model inference; the Cambridge base does not retarget them.

Steve-owned auth/JWKS overrides are `STEVE_AGENT_IDENTITY_AUTHAPI_BASE_URL` and
`STEVE_AGENT_IDENTITY_JWKS_BASE_URL`. Configuration and auth selection govern
when the adapter uses these transports; leaving first-party agent credentials
and the optional `use_agent_identity` feature unused avoids their intended
entry points. This is not a runtime proof that no caller can reach them.
Sources: `codex-rs/agent-identity/src/lib.rs`,
`codex-rs/login/src/auth/agent_identity.rs` and `codex-rs/features/src/lib.rs`.
No identity key, token or registration was read or sent during this static audit.
