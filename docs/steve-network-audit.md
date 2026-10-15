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
first-party backend authentication. Steve does not GET `/ps/plugins/{plugin_id}`
for plugin detail, `/ps/plugins/workspace/shared`, or `/ps/plugins/installed`.
Steve does not GET `/ps/plugins/{plugin_id}/skills/{skill_name}`.
Authentication and product SKU accompany the remaining plugin-service requests.

Steve does not POST `/ps/plugins/{plugin_id}/install` or
`/ps/plugins/{plugin_id}/uninstall`.
Local plugin interoperability does not require mechanically renaming the
external service paths. Avoid
first-party remote plugin operations and inspect installed plugin transports
when preventing OpenAI traffic. Analytics and feedback flags do not gate the
reviewed request functions. No plugin-service request was made for this audit.
Source: `codex-rs/core-plugins/src/remote.rs`.

### Plugin sharing uploads

Steve does not POST `/public/plugins/workspace/upload-url`, PUT an archive to a
returned upload URL, or POST `/public/plugins/workspace`. It does not PUT
`/ps/plugins/{id}/shares`, DELETE `/public/plugins/workspace/{id}`, or GET
`/ps/plugins/workspace/created`. A previously recorded local share-path mapping
can still be read when listing local plugins.

### Plugin bundle downloads

Steve does not GET a remote plugin bundle download URL. npm source can still
unpack a local plugin archive.

## Realtime voice

Realtime setup starts from the selected provider. WebSocket and WebRTC call
base URLs can be overridden separately by
`experimental_realtime_ws_base_url` and
`experimental_realtime_webrtc_call_base_url`; an existing call can also supply
its sideband base. These overrides need inspection even when ordinary inference
uses Cambridge. The WebRTC sideband does not default to
`https://api.openai.com/v1`. It connects only when an explicit sideband base
URL is set. This source routing does not establish Cambridge support for
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

Steve does not GET `/connectors/directory/list` or
`/connectors/directory/list_workspace`. It does not POST `/ps/apps/batch`. It
does not construct a hosted Apps MCP client at `{chatgpt_base_url}/ps/mcp`.
A previously recorded connector directory or app-metadata cache can still be
read. `app/list` and `app/read` use that cache and local plugin manifests.
An explicit `mcp_servers` URL stays in use, including a user-configured
`codex_apps` server. `features.apps=false` still omits the server named
`codex_apps`. This does not prove that every explicitly configured or
plugin-registered MCP endpoint is suppressed. Sources:
`codex-rs/chatgpt/src/connectors.rs`,
`codex-rs/connectors/src/lib.rs`, `codex-rs/codex-mcp/src/mcp/mod.rs` and
`codex-rs/features/src/lib.rs`.

## Remote control

Steve does not POST `wham/remote/control/server/enroll`,
`wham/remote/control/server/refresh`, `wham/remote/control/server/pair`, or
`wham/remote/control/server/pair/status`. It does not GET or DELETE
`wham/remote/control/environments/{id}/clients`. It does not open a WebSocket
to `wham/remote/control/server`. A daemon settings file may still contain
`remoteControlEnabled`. That value is stored and is not used to open a relay.

Changing the inference provider does not change every service endpoint.
Disabling analytics and feedback does not disable MCP requests or explicitly
configured OpenTelemetry collectors.

## Credential broker

Steve's credential broker does not recognize `OPENAI_API_KEY`, does not bind
`api.openai.com`, and does not inject that key. GitHub credentials and
user-configured credential providers still virtualize and inject on their own
hosts. An explicit model base URL stays in use. Sources:
`codex-rs/network-proxy/src/credential_broker/providers.rs` and
`codex-rs/network-proxy/src/credential_broker/providers/github.rs`.

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

Plugin list does not GET `{chatgpt_base_url}/plugins/featured`. Featured
plugin ids are empty. An explicit `chatgpt_base_url` is left unchanged. Steve
does not fetch `https://github.com/openai/plugins.git`, the GitHub archive of
that repository, or `https://chatgpt.com/backend-api/plugins/export/curated`.
A curated snapshot already stored under the Steve home is still read locally.
User-configured Git marketplaces remain a separate client. Local plugin
loading stays. Legacy plugin install and uninstall do not POST
`{chatgpt_base_url}/plugins/{id}/enable` or `/uninstall`. Plugin search does
not GET `{chatgpt_base_url}/ps/plugins/search`. Local marketplace search
stays. Plugin recommendations do not GET
`{chatgpt_base_url}/ps/plugins/suggested/codex`. The plugin directory does not
GET `{chatgpt_base_url}/ps/plugins/list`. A cached directory may still be
read. Installed plugins do not GET `{chatgpt_base_url}/ps/plugins/installed`.
An in-memory installed snapshot may still be read. Shared workspace plugins
do not GET `{chatgpt_base_url}/ps/plugins/workspace/shared`. Plugin detail does
not GET `{chatgpt_base_url}/ps/plugins/{plugin_id}`. Steve does not GET
`/ps/plugins/{plugin_id}/skills/{skill_name}`. Steve does not POST
`{chatgpt_base_url}/ps/plugins/{plugin_id}/install` or
`{chatgpt_base_url}/ps/plugins/{plugin_id}/uninstall`. Share and bundle
clients are separate.

Raw local evidence: `~/.cache/codex-cambridge-build/steve-network-smoke-n05wyupc/`
and `steve-network-smoke-cey30wqi/` contain configuration, structured model
output, exit status and syscall traces. The tested archive predates later
daemon diagnostic wording changes. Sources:
`codex-rs/core-plugins/src/startup_sync.rs`, `manager.rs`, and
`codex-rs/app-server/src/request_processors/plugins/search.rs`.

## Config bundle

Steve does not GET `/wham/config/bundle` or `/api/codex/config/bundle`. Startup
does not substitute `https://chatgpt.com/backend-api/` to fetch a cloud config
bundle. Local configuration still loads.

## Rate-limit usage

Steve does not GET `/wham/usage` or `/api/codex/usage`. Longer usage paths,
including thread usage, turn cost, and plan history, are separate requests.
Steve does not GET `/wham/rate-limit-reset-credits` or
`/api/codex/rate-limit-reset-credits`, and does not POST either path with
`/consume`.

## Accounts check

Steve does not GET `/wham/accounts/check` or `/api/codex/accounts/check`.
Account reads still return the local account. An explicit configured base URL
stays in use. Steve does not POST
`/wham/accounts/send_add_credits_nudge_email` or
`/api/codex/accounts/send_add_credits_nudge_email`. Steve does not GET
`/wham/profiles/me` or `/api/codex/profiles/me`. Thread usage remains a
separate request. Steve does not GET `/wham/settings/user` or
`/api/codex/settings/user`. Steve does not GET `/wham/workspace-messages` or
`/api/codex/workspace-messages`.

## Cloud-task submission

Cloud-task initialization does not default to
`https://chatgpt.com/backend-api`. `STEVE_CLOUD_TASKS_BASE_URL` is required,
and an explicit value is used after the existing host check. Cambridge
inference configuration does not select this backend. The CLI apply command is gone. Steve does not GET
`{chatgpt_base_url}/wham/tasks/{task_id}` from that command.
Steve does not GET `/wham/tasks/list` or `/api/codex/tasks/list`, and does not GET `/wham/tasks/{id}` or `/api/codex/tasks/{id}`. Steve does not POST `/wham/tasks` or `/api/codex/tasks`. Steve does not GET `/wham/tasks/{id}/turns/{turn}/sibling_turns` or `/api/codex/tasks/{id}/turns/{turn}/sibling_turns`. Environment discovery sends GitHub repository owner/name in request paths and
uses configured authentication headers. This section
does not establish every entry point or audit other cloud-task operations.
Sources: `codex-rs/cloud-tasks/src/lib.rs`,
`codex-rs/cloud-tasks/src/env_detect.rs` and
`codex-rs/cloud-tasks-client/src/http.rs`.

| Mechanism | Destination | Data leaving the client | Control |
| --- | --- | --- | --- |
| Inference | Selected provider's explicit `base_url` | Prompts, conversation context, tool definitions and results, model/reasoning settings, request metadata and authentication | A missing base URL is not replaced with `api.openai.com` or `chatgpt.com`. An explicit base URL is used as configured. |
| Model discovery | Provider `/models` or an explicit catalog URL | Authentication, configured headers/query parameters and client version; receives model definitions and ETag | A missing base URL is not replaced with the ChatGPT Codex catalog. Cache/refresh policy also controls fetching. |
| Built-in Statsig metrics | none | A configured `statsig` metrics exporter does not create a client or send metrics. | The config value still parses. Explicit OTLP collectors are unchanged. |
| Explicit OTLP | Configured HTTP/gRPC endpoints | Configured metrics, logs or traces and resource attributes; content depends on instrumentation and logging settings | Configure the individual exporters. Analytics consent does not override these explicitly configured collectors. |
| App-server analytics | none | Steve does not POST `{chatgpt_base_url}/codex/analytics-events/events`. | `[analytics].enabled` still parses. A debug capture file can record events locally. |
| Feedback | none | Steve does not submit feedback envelopes to the Sentry DSN. | `[feedback].enabled` still parses. Local feedback log capture remains. |
| Login and token refresh | none | Steve does not build `auth.openai.com` authorize, token, revoke, device-code, or personal-access-token metadata requests, and it does not select `auth.api.openai.org` for workload identity. | Stored and explicitly supplied credentials still load. Browser and device-code ChatGPT login are unavailable. |
| OpenAI docs MCP | Configured `https://developers.openai.com/mcp`, attributed as `?source=codex` | Initialization identity/capabilities, tool names/arguments, configured authentication and MCP metadata; receives documentation and protocol results | This wrapper does not register the server. Leaving it unconfigured prevents this configured path. Analytics/feedback settings do not disable MCP metadata. |

A documentation request is still an outbound request. MCP metadata can include
call/thread/session IDs, model and reasoning settings, sandbox/review settings,
turn timing, workspace Git metadata and tracing context. Normal MCP metadata
removes some agent lineage and harness inventory fields; host-owned Apps have a
separate lineage path. A read-only tool is not a guarantee that no session
information leaves the client.

Steve does not build or submit a Sentry feedback envelope. Local log capture
remains on the process.

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
- Feedback log capture: `codex-rs/feedback/src/lib.rs` and
  `codex-rs/app-server/src/request_processors/feedback_processor.rs`.
- MCP attribution and metadata:
  `codex-rs/codex-mcp/src/openai_docs_source_attribution.rs`,
  `codex-rs/codex-mcp/src/rmcp_client.rs`,
  `codex-rs/rmcp-client/src/rmcp_client.rs`,
  `codex-rs/rmcp-client/src/trace_context.rs`,
  `codex-rs/core/src/mcp_tool_call.rs`, `codex-rs/core/src/turn_metadata.rs` and
  `codex-rs/core/src/responses_metadata.rs`.

A Cambridge profile with an explicit Cambridge base URL redirects ordinary
inference and model discovery to that URL. Steve does not POST analytics events
or submit a Sentry feedback envelope. Steve does not exchange login or refresh
tokens with auth.openai.com. The auth manager still loads stored and explicitly
supplied credentials. These settings do not disable plugin acquisition
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

The macOS desktop command opens an installed local desktop application.
If none is installed and `--download-url` is unset, Steve reports that the
desktop app is not installed and does not request `persistent.oaistatic.com`.
An explicit download URL is fetched with curl. After signature verification,
an installed or downloaded application is opened with
`codex://threads/new?path=...`. Source: `codex-rs/cli/src/desktop_app/mac.rs`
and `codex-rs/cli/src/desktop_installer_url.rs`.

Doctor does not GET a desktop update manifest. It does not request an
appcast or Windows store file from `persistent.oaistatic.com`, and it does
not request `https://chatgpt.com/backend-api/wham/app/appcast`. A locally
staged macOS Sparkle bundle can still be reported. When configuration fails
to load, doctor does not probe `https://chatgpt.com/backend-api/codex/responses`.
An API-key provider with no base URL is not probed at `https://api.openai.com/v1`.
An explicit provider base URL is still probed. A loaded configuration's
`chatgpt_base_url` is still used for the ChatGPT reachability probe. The TUI
does not GET `{chatgpt_base_url}/wham/security-setup`. Source:
`codex-rs/cli/src/doctor/updates.rs`, `codex-rs/cli/src/doctor.rs`, and
`codex-rs/tui/src/security_setup.rs`.

## Guardian Decisions comparison

Guardian scoring does not build or send a request to
`https://api.openai.com/v1/decisions`. The feature key
`guardianv2_decisions_comparison` remains so older configuration files parse;
its stage is removed and no code reads it. Baseline Guardian classification
still uses the configured model provider. Source:
`codex-rs/features/src/lib.rs` and
`codex-rs/ext/guardian-v2/src/async_scorer/`.

## Responses API proxy

`steve-responses-api-proxy` forwards `POST /v1/responses` to the absolute URL
given by `--upstream-url`. Omitting the flag is an error. Steve does not
select `https://api.openai.com/v1/responses`. An explicit upstream, including
Cambridge or an Azure deployment URL, is forwarded unchanged. Source:
`codex-rs/responses-api-proxy/src/lib.rs`.

## Built-in pet assets

Loading a selected built-in pet checks its versioned cache under STEVE_HOME.
A missing or invalid spritesheet is an error. Steve does not GET
`https://persistent.oaistatic.com/codex/pets/v1`. Custom pets remain local
files. Sources: `codex-rs/tui/src/pets/asset_pack.rs` and
`codex-rs/tui/src/pets/mod.rs`.

## Browser and device-code login

Steve does not build an `https://auth.openai.com/oauth/authorize` URL, and it
does not exchange an authorization code at `/oauth/token`. CLI and app-server
ChatGPT login entry points return an error without constructing those URLs and
without clearing stored credentials. Device login does not POST
`/api/accounts/deviceauth/usercode` or `/api/accounts/deviceauth/token`.
Sources: `codex-rs/cli/src/login.rs` and
`codex-rs/app-server/src/request_processors/account_processor.rs`.

## Managed token refresh and revocation

Stored ChatGPT credentials still load. Refreshing them does not POST the refresh
token to `https://auth.openai.com/oauth/token`, including when
`STEVE_REFRESH_TOKEN_URL_OVERRIDE` is set. Logout removes local auth and does
not POST `https://auth.openai.com/oauth/revoke`. An explicit gateway token URL
remains a separate configured OAuth client. Source:
`codex-rs/login/src/auth/manager.rs`.

## Personal access token metadata

A supplied or stored personal access token is held locally. Steve does not GET
`https://auth.openai.com/api/accounts/v1/user-auth-credential/whoami`, so email,
user id, account id, plan and FedRAMP metadata from that endpoint are absent.
A forced workspace cannot be proven from the token alone and fails closed
without a request. Sources: `codex-rs/login/src/auth/personal_access_token.rs`
and `codex-rs/login/src/auth/manager.rs`.

## Workload identity

`OPENAI_FEDERATION_RULE_ID` or `OPENAI_IDENTITY_TOKEN_FILE` still marks workload
identity as selected, and logout stays refused while that marker is set. The
login adapter does not choose `https://auth.openai.com/oauth/token` or
`https://auth.api.openai.org/oauth/token` and does not construct a
workload-identity token exchange. Source:
`codex-rs/login/src/auth/workload_identity.rs`.

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
