# Bazel in codex-rs

This repository uses Bazel to build the Rust workspace under `codex-rs`.
Cargo remains the source of truth for crates and features, while Bazel
provides hermetic builds, toolchains, and cross-platform artifacts.

As of 6/1/2026, this setup is still experimental as we stabilize it.

## High-level layout

- `../MODULE.bazel` defines Bazel dependencies and Rust toolchains.
- `rules_rs` imports third-party crates from `codex-rs/Cargo.toml` and
  `codex-rs/Cargo.lock` via `crate.from_cargo(...)` and exposes them under
  `@crates`.
- `../defs.bzl` provides `codex_rust_crate`, which wraps `rust_library`,
  `rust_binary`, and `rust_test` so Bazel targets line up with Cargo conventions.
  It provides a sane set of defaults that work for most first-party crates, but may
  need tweaks in some cases.
- Each crate in `codex-rs/*/BUILD.bazel` typically uses `codex_rust_crate` and
  makes some adjustments if the crate needs additional compile-time or runtime data,
  or other customizations.

## Running Bazel locally

The repository root `justfile` exposes the common Bazel entry points:

```bash
just bazel-test
just bazel-clippy
```

Ordinary local `bazel` and `just` invocations run locally. BuildBuddy cache,
build event upload, downloads, and remote execution require explicit endpoints.

## BuildBuddy

Codex uses BuildBuddy for a shared Bazel cache and remoted builds and tests. To use it
to speed up your builds and tests you'll need to provide an API key and select a
configuration.

### BuildBuddy API key

Create a BuildBuddy API key as described in BuildBuddy's [Authentication Guide][bb-auth-guide],
then add it to `~/.bazelrc`:

```bazelrc
# Local machine only; this file contains a BuildBuddy credential.
common --remote_header=x-buildbuddy-api-key=<your-buildbuddy-api-key>
```

Keeping the credential outside the workspace reduces the risk of accidentally
committing it.

If you need different API keys for different projects, put the API key in
`%workspace%/user.bazelrc` instead. The checked-in `.bazelrc` optionally imports
that file, and `.gitignore` excludes it. Do not commit or share a file containing
the credential.

[bb-auth-guide]: https://www.buildbuddy.io/docs/guide-auth/#managing-keys

### Selecting remote services

Supply the endpoints for the services you want in `%workspace%/user.bazelrc`,
`~/.bazelrc`, or Bazel command-line flags. Cache, build events, downloads, and
execution are separate choices. For example, replace these placeholders with
endpoints you have selected:

```bazelrc
common --remote_cache=grpcs://<cache-host>
common --bes_backend=grpcs://<events-host>
common --bes_results_url=https://<results-host>/invocation/
common --experimental_remote_downloader=grpcs://<downloader-host>
```

For remote execution, also supply its endpoint and select the existing
endpoint-free execution settings:

```bazelrc
common --remote_executor=grpcs://<executor-host>
common --config=remote
```

GitHub Actions routes Bazel build and output-resolution commands through
`.github/scripts/run_bazel_with_buildbuddy.py`. Higher-level helpers such as
`.github/scripts/run-bazel-ci.sh` and `.github/scripts/rusty_v8_bazel.py`
use that wrapper. A present `BUILDBUDDY_API_KEY` supplies the existing remote
authentication header; neither the key nor the GitHub repository selects a host.
The wrapper also normalizes startup options and local cache paths.

With a key, the existing `ci-linux`, `ci-macos`, `ci-v8`, and
`ci-windows-cross` arguments select `--config=remote` before later caller
options, preserving platform and local-test overrides. `ci-windows` and
commands without those CI arguments receive only the credential header.
Without a key, the wrapper removes those four remote CI arguments as before.
Arguments after `--` are program payload and do not select execution settings.

The checked-in configuration supplies no build-service endpoint. Keyed CI
therefore still needs explicit service configuration; the presence of a key
does not establish that remote execution is configured or will succeed.

## Evolving the setup

When you add or change Rust dependencies, update the Cargo.toml/Cargo.lock as normal.
Then refresh the Bzlmod lockfile from the repo root:

```bash
just bazel-lock-update
```

This runs `bazel mod deps --lockfile_mode=update` and updates `MODULE.bazel.lock` if needed.
Commit the lockfile changes along with your Cargo lockfile update.

To verify lockfile alignment locally (the same check CI runs), use:

```bash
just bazel-lock-check
```

In some cases, an upstream crate may need a patch or a `crate.annotation` in `../MODULE.bzl`
to have it build in Bazel's sandbox or make it cross-compilation-friendly. If you see issues,
feel free to ping zbarsky or mbolin.

When you add a new crate or binary:

1. Add it to the Cargo workspace as usual.
2. Create a `BUILD.bazel` that calls `codex_rust_crate` (see nearby crates for
   examples).
3. If a dependency needs special handling (compile/runtime data, additional binaries
   for integration tests, env vars, etc) you may need to adjust the parameters to
   `codex_rust_crate` to configure it.
   One common customization is setting `test_tags = ["no-sandbox]` to run the test
   unsandboxed. Prefer to avoid it, but it is necessary in some cases such as when the
   test itself uses Seatbelt (the sandbox does as well, and it cannot be nested).
   To limit the blast radius, consider isolating such tests to a separate crate.

If you see build issue and are not sure how to apply the proper customizations, feel free to ping zbarsky or mbolin.

## References

- Bazel overview: https://bazel.build/
- Bzlmod (module system): https://bazel.build/external/overview
- rules_rust: https://github.com/bazelbuild/rules_rust
- rules_rs: https://github.com/bazelbuild/rules_rs
