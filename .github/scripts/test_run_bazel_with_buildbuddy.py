#!/usr/bin/env python3

import os
import subprocess
import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

import run_bazel_with_buildbuddy


class RunBazelWithBuildBuddyTest(unittest.TestCase):
    def test_keyless_invocation_drops_remote_ci_configuration(self) -> None:
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                ["build", "--config=ci-linux", "--", "//codex-rs/cli:codex"],
                {},
            ),
            ["build", "--", "//codex-rs/cli:codex"],
        )

    def test_program_arguments_after_separator_do_not_select_or_lose_rbe(self) -> None:
        args = ["run", "//codex-rs/cli:codex", "--", "--config=ci-linux"]
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(args, {}),
            args,
        )
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                args, {"BUILDBUDDY_API_KEY": "token"}
            ),
            ["run", "--remote_header=x-buildbuddy-api-key=token", *args[1:]],
        )

    def test_key_supplies_credentials_without_selecting_a_host(self) -> None:
        identities = [
            {},
            {"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": "openai/codex", "GITHUB_EVENT_NAME": "push"},
            {"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": "example/steve", "GITHUB_EVENT_NAME": "pull_request"},
            {"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": "openai/codex", "GITHUB_EVENT_NAME": "pull_request", "GITHUB_EVENT_PATH": "missing-event.json"},
        ]
        for identity in identities:
            for ci in (None, "ci-linux", "ci-macos", "ci-v8", "ci-windows-cross", "ci-windows"):
                with self.subTest(identity=identity, ci=ci):
                    caller_args = [
                        *([f"--config={ci}"] if ci else []),
                        "--remote_cache=grpc://127.0.0.1:12345",
                        "--remote_executor=grpc://127.0.0.1:12346",
                        "--extra_execution_platforms=//:caller_platform",
                        "--", "literal spaced value",
                    ]
                    remote_args = (
                        ["--config=remote"]
                        if ci in ("ci-linux", "ci-macos", "ci-v8", "ci-windows-cross")
                        else []
                    )
                    self.assertEqual(
                        run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                            ["build", *caller_args],
                            {"BUILDBUDDY_API_KEY": "token", **identity},
                        ),
                        ["build", *remote_args, "--remote_header=x-buildbuddy-api-key=token", *caller_args],
                    )

    def test_key_only_adds_header_without_explicit_endpoint(self) -> None:
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                ["build", "//codex-rs/cli:codex"],
                {"BUILDBUDDY_API_KEY": "token"},
            ),
            ["build", "--remote_header=x-buildbuddy-api-key=token", "//codex-rs/cli:codex"],
        )

    def test_remote_ci_shape_does_not_require_an_endpoint_on_argv(self) -> None:
        # Bazel may load endpoints from user.bazelrc or the home rc.
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                ["build", "--config=ci-windows-cross", "//codex-rs/cli:codex"],
                {"BUILDBUDDY_API_KEY": "token"},
            ),
            [
                "build", "--config=remote", "--remote_header=x-buildbuddy-api-key=token",
                "--config=ci-windows-cross", "//codex-rs/cli:codex",
            ],
        )

    def test_query_credentials_are_inserted_before_expression(self) -> None:
        expression = 'kind("rust_library rule", //codex-rs/...)'
        for command in ("query", "cquery", "aquery"):
            with self.subTest(command=command):
                self.assertEqual(
                    run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                        [command, "--output=label", expression],
                        {"BUILDBUDDY_API_KEY": "token"},
                    ),
                    [command, "--remote_header=x-buildbuddy-api-key=token", "--output=label", expression],
                )

    def test_checked_in_rc_has_no_build_service_endpoints(self) -> None:
        rc = (Path(__file__).resolve().parents[2] / ".bazelrc").read_text()
        for option in ("--bes_backend=", "--bes_results_url=", "--remote_cache=", "--experimental_remote_downloader=", "--remote_executor="):
            self.assertNotIn(option, rc)
        self.assertIn("common:remote --strategy=remote", rc)
        self.assertIn("try-import %workspace%/user.bazelrc", rc)
        self.assertNotIn("common:buildbuddy-", rc)

    def test_bazel_command_uses_configured_binary_locally(self) -> None:
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "info",
                "execution_root",
                env={"CODEX_BAZEL_BIN": "fake-bazel"},
            ),
            ["fake-bazel", "info", "execution_root"],
        )

    def test_bazel_command_normalizes_github_actions_startup_options(self) -> None:
        env = {
            "BAZEL_OUTPUT_USER_ROOT": "/tmp/bazel-output",
            "GITHUB_ACTIONS": "true",
        }

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command("build", "//codex-rs/...", env=env),
            [
                "bazel",
                "--output_user_root=/tmp/bazel-output",
                "--noexperimental_remote_repo_contents_cache",
                "build",
                "//codex-rs/...",
            ],
        )
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "--experimental_remote_repo_contents_cache",
                "build",
                "//codex-rs/...",
                env=env,
            ),
            [
                "bazel",
                "--output_user_root=/tmp/bazel-output",
                "--experimental_remote_repo_contents_cache",
                "build",
                "//codex-rs/...",
            ],
        )

    def test_bazel_command_uses_configured_local_caches(self) -> None:
        env = {
            "BAZEL_REPO_CONTENTS_CACHE": "/tmp/bazel-repo-contents",
            "BAZEL_REPOSITORY_CACHE": "/tmp/bazel-repository",
        }

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "build",
                "--config=local",
                "//codex-rs/...",
                env=env,
            ),
            [
                "bazel",
                "build",
                "--config=local",
                "//codex-rs/...",
                "--repo_contents_cache=/tmp/bazel-repo-contents",
                "--repository_cache=/tmp/bazel-repository",
            ],
        )

    def test_bazel_command_adds_local_caches_before_separator(self) -> None:
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "build",
                "//codex-rs/...",
                "--",
                "--program-arg",
                env={"BAZEL_REPOSITORY_CACHE": "/tmp/bazel-repository"},
            ),
            [
                "bazel",
                "build",
                "//codex-rs/...",
                "--repository_cache=/tmp/bazel-repository",
                "--",
                "--program-arg",
            ],
        )

    def test_main_preserves_spaced_argument_and_child_exit_status(self) -> None:
        spaced_arg = (
            r"--test_env=PATH=C:\Program Files\PowerShell\7;C:\Program Files\Git\bin"
        )
        child_code = (
            f"import sys; sys.exit(37 if sys.argv[1] == {spaced_arg!r} else 91)"
        )
        env = os.environ.copy()
        env["CODEX_BAZEL_BIN"] = sys.executable
        env.pop("BAZEL_OUTPUT_USER_ROOT", None)
        env.pop("BUILDBUDDY_API_KEY", None)
        env.pop("GITHUB_ACTIONS", None)

        result = subprocess.run(
            [
                sys.executable,
                str(Path(run_bazel_with_buildbuddy.__file__)),
                "-c",
                child_code,
                spaced_arg,
            ],
            env=env,
            check=False,
            capture_output=True,
            text=True,
        )

        self.assertEqual(result.returncode, 37, result.stderr)


if __name__ == "__main__":
    unittest.main()
