#!/usr/bin/env python3

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import textwrap
import unittest
from unittest.mock import patch


INSTALL_SCRIPT = Path(__file__).with_name("install.sh")
VERSION = "0.142.5"
MISMATCH_VERSION = "0.145.0"


class InstallShTest(unittest.TestCase):
    def test_corrupt_github_assets_fail_without_selecting_a_release(self) -> None:
        for fault in ("archive", "checksum", "missing_manifest_entry"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as temp_dir:
                root = Path(temp_dir)
                archive, checksum, metadata = create_package_release(root)
                if fault == "archive":
                    archive.write_bytes(b"corrupt archive\n")
                elif fault == "checksum":
                    checksum.write_bytes(b"corrupt checksum\n")
                else:
                    checksum.write_text(f"{'a' * 64}  other-platform.tar.gz\n")
                    data = json.loads(metadata)
                    for asset in data["assets"]:
                        if asset["name"] == checksum.name:
                            asset["digest"] = "sha256:" + hashlib.sha256(checksum.read_bytes()).hexdigest()
                    metadata = json.dumps(data)

                result, requests = run_installer_in(
                    root,
                    VERSION,
                    metadata_json=metadata,
                    archive_path=archive,
                    checksum_path=checksum,
                    force_macos=True,
                )

                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(all("handley-lab/steve" in url for url in requests))
                self.assertFalse((root / "install-bin" / "steve").exists())
                self.assertFalse((root / "steve-home/packages/standalone/current").is_symlink())
                self.assertIn("checksum" if fault != "missing_manifest_entry" else "Could not find SHA-256 digest", result.stderr)

    def test_install_leaves_codex_command_and_home_untouched(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)
            old_home = root / "codex-home"
            old_home.mkdir()
            old_config = old_home / "config.toml"
            old_config.write_bytes(b"Codex configuration canary\n")
            old_bin = root / "bin" / "codex"
            old_bin.parent.mkdir()
            canary = "#!/usr/bin/env node\nthrow new Error('Codex must not run');\n"
            write_executable(old_bin, canary)

            with patch.dict(os.environ, {"CODEX_HOME": str(old_home), "CODEX_INSTALL_DIR": str(old_bin.parent)}):
                result, requests = run_installer_in(
                    root,
                    VERSION,
                    metadata_json=metadata_json,
                    archive_path=archive_path,
                    checksum_path=checksum_path,
                    force_macos=True,
                )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(old_bin.read_text(), canary)
            self.assertEqual(old_config.read_bytes(), b"Codex configuration canary\n")
            self.assertEqual(list(old_home.iterdir()), [old_config])
            self.assertNotIn(str(old_bin), result.stdout)
            self.assertTrue((root / "install-bin" / "steve").is_symlink())
            self.assertTrue(all("handley-lab/steve" in url for url in requests))

    def test_metadata_fetch_failure_is_not_reported_as_missing_assets(self) -> None:
        result, requests = run_installer(VERSION, metadata_failure=True)

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            requests,
            [
                "https://api.github.com/repos/handley-lab/steve/releases/tags/"
                f"rust-v{VERSION}"
            ],
        )
        self.assertIn(
            f"Could not fetch GitHub release metadata for Steve {VERSION}",
            result.stderr,
        )
        self.assertNotIn("Could not find Steve package", result.stderr)

    def test_exact_release_uses_github_metadata_once(self) -> None:
        result, requests = run_installer(VERSION)

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            requests,
            [
                "https://api.github.com/repos/handley-lab/steve/releases/tags/"
                f"rust-v{VERSION}",
                "https://github.com/handley-lab/steve/releases/download/"
                f"rust-v{VERSION}/steve-package_SHA256SUMS",
            ],
        )
        self.assertIn(f"Resolved version: {VERSION}", result.stdout)

    def test_alpha_hotfix_release_is_valid(self) -> None:
        version = "0.145.0-alpha.23.1"
        result, requests = run_installer(version)

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            requests,
            [
                "https://api.github.com/repos/handley-lab/steve/releases/tags/"
                f"rust-v{version}",
                "https://github.com/handley-lab/steve/releases/download/"
                f"rust-v{version}/steve-package_SHA256SUMS",
            ],
        )
        self.assertIn(f"Resolved version: {version}", result.stdout)

    def test_latest_release_reuses_version_metadata(self) -> None:
        result, requests = run_installer("latest")

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            requests,
            [
                "https://api.github.com/repos/handley-lab/steve/releases/latest",
                "https://github.com/handley-lab/steve/releases/download/"
                f"rust-v{VERSION}/steve-package_SHA256SUMS",
            ],
        )
        self.assertIn(f"Resolved version: {VERSION}", result.stdout)

    def test_compact_metadata_is_independent_of_field_order(self) -> None:
        result, requests = run_installer(
            "latest", metadata_json=release_metadata(compact=True, reorder=True)
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            requests,
            [
                "https://api.github.com/repos/handley-lab/steve/releases/latest",
                "https://github.com/handley-lab/steve/releases/download/"
                f"rust-v{VERSION}/steve-package_SHA256SUMS",
            ],
        )
        self.assertIn(f"Resolved version: {VERSION}", result.stdout)

    def test_json_like_strings_and_nested_fields_do_not_define_assets(self) -> None:
        result, requests = run_installer(
            VERSION, metadata_json=legacy_release_metadata_with_decoys()
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(requests), 2)
        self.assertIn("/steve-npm-", requests[1])
        self.assertNotIn("steve-package_SHA256SUMS", requests[1])

    def test_macos_install_exposes_code_mode_host_beside_codex(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)

            result, _requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            install_bin = root / "install-bin"
            current = root / "steve-home" / "packages" / "standalone" / "current"
            codex_path = install_bin / "steve"
            host_path = install_bin / "steve-code-mode-host"
            self.assertEqual(os.readlink(codex_path), str(current / "bin" / "steve"))
            self.assertEqual(
                os.readlink(host_path),
                str(current / "bin" / "steve-code-mode-host"),
            )
            self.assertTrue(os.access(host_path, os.X_OK))

    def test_latest_installs_verified_github_package_by_default(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)

            result, requests = run_installer_in(
                root,
                "latest",
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                requests,
                [
                    "https://api.github.com/repos/handley-lab/steve/releases/latest",
                    f"https://github.com/handley-lab/steve/releases/download/rust-v{VERSION}/steve-package_SHA256SUMS",
                    f"https://github.com/handley-lab/steve/releases/download/rust-v{VERSION}/steve-package-aarch64-apple-darwin.tar.gz",
                ],
            )

    def test_daemon_install_preserves_visible_cli_and_profile(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            archive, checksum, metadata = create_package_release(root)
            options = dict(
                metadata_json=metadata,
                archive_path=archive,
                checksum_path=checksum,
                force_macos=True,
            )
            pinned, _ = run_installer_in(root, VERSION, **options)
            self.assertEqual(pinned.returncode, 0, pinned.stderr)
            visible = root / "install-bin/steve"
            original = visible.resolve()
            pending = visible.with_name(".steve.12345")
            pending.symlink_to(original)
            profile = root / "home/.profile"
            profile.write_text("unchanged profile\n")

            installed, _ = run_installer_in(root, "latest", daemon_only=True, **options)
            self.assertEqual(installed.returncode, 0, installed.stderr)
            daemon = root / "steve-home/packages/app-server-daemon"
            release_name = f"{VERSION}-aarch64-apple-darwin"
            self.assertEqual(
                (daemon / "current").resolve(), daemon / "releases" / release_name
            )
            self.assertEqual((daemon / "auto-update-version").read_text(), release_name)
            history = daemon / "selection-history"
            original_history = history.read_bytes()
            self.assertEqual(history.read_text().splitlines()[:2], [release_name, ""])
            repeated, _ = run_installer_in(root, "latest", daemon_only=True, **options)
            self.assertEqual(repeated.returncode, 0, repeated.stderr)
            self.assertEqual(history.read_bytes(), original_history)
            options["daemon_only"] = True
            local = daemon / "releases/local-development"
            local.mkdir()
            (daemon / "current").unlink()
            (daemon / "current").symlink_to(local)
            marker = daemon / "auto-update-version"
            marker.unlink()

            scheduled, _ = run_installer_in(
                root, "latest", update_guard_from_release=local.name, **options
            )
            self.assertEqual(scheduled.returncode, 0, scheduled.stderr)
            self.assertEqual((daemon / "current").resolve(), local)
            self.assertFalse(marker.exists())

            raced, _ = run_installer_in(
                root,
                "latest",
                update_guard_from_release="a-different-release",
                manual_update=True,
                **options,
            )
            self.assertNotEqual(raced.returncode, 0, raced.stderr)
            self.assertEqual((daemon / "current").resolve(), local)
            self.assertFalse(marker.exists())

            binary = daemon / "releases" / release_name / "bin/steve"
            contents = binary.read_text()
            binary.write_text(
                '#!/bin/sh\n[ "$1" = "--version" ] || exit 2\n'
                f"echo steve-cli {VERSION}\n"
            )
            incompatible, _ = run_installer_in(
                root,
                "latest",
                update_guard_from_release=local.name,
                manual_update=True,
                **options,
            )
            self.assertNotEqual(incompatible.returncode, 0)
            self.assertIn("does not support daemon-owned packages", incompatible.stderr)
            self.assertEqual((daemon / "current").resolve(), local)
            self.assertFalse(marker.exists())
            binary.write_text(contents)

            restored, _ = run_installer_in(
                root,
                "latest",
                update_guard_from_release=local.name,
                manual_update=True,
                **options,
            )
            self.assertEqual(restored.returncode, 0, restored.stderr)
            self.assertEqual(
                (daemon / "current").resolve(), daemon / "releases" / release_name
            )
            self.assertEqual(marker.read_text(), release_name)
            self.assertEqual(visible.resolve(), original)
            self.assertTrue(pending.is_symlink())
            self.assertEqual(profile.read_text(), "unchanged profile\n")
            self.assertFalse(
                (root / "steve-home/packages/standalone/auto-update-version").exists()
            )

    def test_explicit_release_pins_even_the_current_latest_version(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive, checksum, metadata = create_package_release(root)
            options = dict(
                metadata_json=metadata,
                archive_path=archive,
                checksum_path=checksum,
                force_macos=True,
            )
            marker = root / "steve-home/packages/standalone/auto-update-version"
            latest, _ = run_installer_in(root, "latest", **options)
            self.assertEqual(latest.returncode, 0, latest.stderr)
            release_name = f"{VERSION}-aarch64-apple-darwin"
            self.assertEqual(marker.read_text(), release_name)

            pinned, _ = run_installer_in(root, VERSION, **options)
            self.assertEqual(pinned.returncode, 0, pinned.stderr)
            self.assertFalse(marker.exists())

            updater_record = (
                root / "steve-home/app-server-daemon/app-server-updater.pid"
            )
            updater_record.parent.mkdir(parents=True)
            updater_record.write_text(
                json.dumps(
                    {"pid": os.getpid(), "processStartTime": process_start_time()}
                )
            )
            old_updater, _ = run_installer_in(
                root, "latest", old_updater_parent_pid=os.getpid(), **options
            )
            self.assertEqual(old_updater.returncode, 0, old_updater.stderr)
            self.assertFalse(marker.exists())

            skipped, _ = run_installer_in(
                root, "latest", update_guard_from_release=release_name, **options
            )
            self.assertEqual(skipped.returncode, 0, skipped.stderr)
            self.assertFalse(marker.exists())

            updater_record.write_text(
                json.dumps({"pid": os.getpid(), "processStartTime": "stale"})
            )
            latest_again, _ = run_installer_in(
                root, "latest", old_updater_parent_pid=os.getpid(), **options
            )
            self.assertEqual(latest_again.returncode, 0, latest_again.stderr)
            self.assertEqual(marker.read_text(), release_name)

            managed = (
                root
                / f"steve-home/packages/standalone/releases/{release_name}/bin/steve"
            )
            managed.unlink()
            guarded, _ = run_installer_in(
                root,
                "latest",
                update_guard_from_release=release_name,
                **options,
            )
            self.assertEqual(guarded.returncode, 0, guarded.stderr)
            self.assertTrue(managed.exists())

    def test_uninspectable_legacy_updater_does_not_clear_pin(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive, checksum, metadata = create_package_release(root)
            options = dict(
                metadata_json=metadata,
                archive_path=archive,
                checksum_path=checksum,
                force_macos=True,
            )
            pinned, _ = run_installer_in(root, VERSION, **options)
            self.assertEqual(pinned.returncode, 0, pinned.stderr)
            updater_record = (
                root / "steve-home/app-server-daemon/app-server-updater.pid"
            )
            updater_record.parent.mkdir(parents=True)
            updater_record.write_text(
                json.dumps(
                    {"pid": os.getpid(), "processStartTime": process_start_time()}
                )
            )

            attempted, _ = run_installer_in(
                root,
                "latest",
                old_updater_parent_pid=os.getpid(),
                fail_ps=True,
                **options,
            )
            self.assertEqual(attempted.returncode, 0, attempted.stderr)
            self.assertFalse(
                (root / "steve-home/packages/standalone/auto-update-version").exists()
            )








    def test_exact_rejects_wrong_binary_version(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(
                root,
                metadata_version=MISMATCH_VERSION,
            )

            result, requests = run_installer_in(
                root,
                MISMATCH_VERSION,
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(
                requests,
                [
                    f"https://api.github.com/repos/handley-lab/steve/releases/tags/rust-v{MISMATCH_VERSION}",
                    f"https://github.com/handley-lab/steve/releases/download/rust-v{MISMATCH_VERSION}/steve-package_SHA256SUMS",
                    f"https://github.com/handley-lab/steve/releases/download/rust-v{MISMATCH_VERSION}/steve-package-aarch64-apple-darwin.tar.gz",
                ],
            )
            self.assertIn(
                f"did not report expected version {MISMATCH_VERSION}",
                result.stderr,
            )
            self.assertNotIn("installed successfully", result.stdout)

    def test_exact_legacy_package_reuses_offline_install(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, metadata_json = create_legacy_release(root)

            first_result, first_requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                legacy_archive_path=archive_path,
                force_macos=True,
            )

            self.assertEqual(first_result.returncode, 0, first_result.stderr)
            self.assertEqual(
                first_requests,
                [
                    "https://api.github.com/repos/handley-lab/steve/releases/tags/"
                    f"rust-v{VERSION}",
                    "https://github.com/handley-lab/steve/releases/download/"
                    f"rust-v{VERSION}/steve-npm-darwin-arm64-{VERSION}.tgz",
                ],
            )

            (root / "requests.log").unlink()
            second_result, second_requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                force_macos=True,
            )

            self.assertEqual(second_result.returncode, 0, second_result.stderr)
            self.assertEqual(
                second_requests,
                [
                    "https://api.github.com/repos/handley-lab/steve/releases/tags/"
                    f"rust-v{VERSION}",
                ],
            )
            self.assertNotIn("Downloading Steve CLI", second_result.stdout)


def run_installer(
    release: str,
    *,
    metadata_failure: bool = False,
    metadata_json: str | None = None,
) -> tuple[subprocess.CompletedProcess[str], list[str]]:
    with tempfile.TemporaryDirectory() as temp_dir:
        return run_installer_in(
            Path(temp_dir),
            release,
            metadata_failure=metadata_failure,
            metadata_json=metadata_json,
        )


def run_installer_in(
    root: Path,
    release: str,
    *,
    metadata_failure: bool = False,
    metadata_json: str | None = None,
    archive_path: Path | None = None,
    checksum_path: Path | None = None,
    legacy_archive_path: Path | None = None,
    force_macos: bool = False,
    update_guard_from_release: str | None = None,
    old_updater_parent_pid: int | None = None,
    fail_ps: bool = False,
    daemon_only: bool = False,
    manual_update: bool = False,
) -> tuple[subprocess.CompletedProcess[str], list[str]]:
    bin_dir = root / "bin"
    bin_dir.mkdir(exist_ok=True)
    request_log = root / "requests.log"
    fake_curl = bin_dir / "curl"
    fake_curl.write_text(
        textwrap.dedent(
            """\
            #!/bin/sh
            url=""
            output=""
            previous=""
            for arg in "$@"; do
              case "$arg" in
                https://*) url="$arg" ;;
              esac
              if [ "$previous" = "-o" ]; then
                output="$arg"
              fi
              previous="$arg"
            done
            printf '%s\n' "$url" >>"$STEVE_TEST_REQUEST_LOG"

            case "$url" in
              https://api.github.com/*)
                if [ "$STEVE_TEST_METADATA_FAILURE" = "1" ]; then
                  echo "curl: (22) The requested URL returned error: 403" >&2
                  exit 22
                fi
                printf '%s\n' "$STEVE_TEST_METADATA_JSON"
                ;;
              https://github.com/handley-lab/steve/releases/download/*/steve-package_SHA256SUMS)
                if [ -n "$STEVE_TEST_CHECKSUM_PATH" ]; then
                  cp "$STEVE_TEST_CHECKSUM_PATH" "$output"
                else
                  exit 22
                fi
                ;;
              https://github.com/handley-lab/steve/releases/download/*/steve-package-*.tar.gz)
                if [ -n "$STEVE_TEST_ARCHIVE_PATH" ]; then
                  cp "$STEVE_TEST_ARCHIVE_PATH" "$output"
                else
                  exit 22
                fi
                ;;
              https://github.com/handley-lab/steve/releases/download/*/steve-npm-*.tgz)
                if [ -n "$STEVE_TEST_LEGACY_ARCHIVE_PATH" ]; then
                  cp "$STEVE_TEST_LEGACY_ARCHIVE_PATH" "$output"
                else
                  exit 22
                fi
                ;;
              *)
                exit 22
                ;;
            esac
            """
        ),
        encoding="utf-8",
    )
    fake_curl.chmod(0o755)
    if force_macos:
        fake_uname = bin_dir / "uname"
        fake_uname.write_text(
            "#!/bin/sh\n"
            'case "$1" in\n'
            "  -s) printf 'Darwin\\n' ;;\n"
            "  -m) printf 'arm64\\n' ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        fake_uname.chmod(0o755)
    if old_updater_parent_pid is not None:
        fake_ps = bin_dir / "ps"
        fake_ps.write_text(
            "#!/bin/sh\nexit 1\n"
            if fail_ps
            else "#!/bin/sh\n"
            'case "$*" in\n'
            '  *lstart*) printf "S %s\\n" "$STEVE_TEST_PARENT_START" ;;\n'
            '  *) printf "%s\\n" "$STEVE_TEST_PARENT_PID" ;;\n'
            "esac\n",
            encoding="utf-8",
        )
        fake_ps.chmod(0o755)

    home = root / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(
        {
            "STEVE_HOME": str(root / "steve-home"),
            "STEVE_INSTALL_DIR": str(root / "install-bin"),
            "STEVE_NON_INTERACTIVE": "1",
            "STEVE_RELEASE": release,
            "STEVE_INSTALL_DAEMON_ONLY": "1" if daemon_only else "0",
            "STEVE_TEST_ARCHIVE_PATH": str(archive_path or ""),
            "STEVE_TEST_CHECKSUM_PATH": str(checksum_path or ""),
            "STEVE_TEST_LEGACY_ARCHIVE_PATH": str(legacy_archive_path or ""),
            "STEVE_TEST_METADATA_FAILURE": "1" if metadata_failure else "0",
            "STEVE_TEST_METADATA_JSON": (
                metadata_json if metadata_json is not None else release_metadata()
            ),
            "STEVE_TEST_REQUEST_LOG": str(request_log),
            "HOME": str(home),
            "PATH": f"{bin_dir}:/usr/bin:/bin",
            "SHELL": "/bin/sh",
        }
    )
    env["STEVE_INSTALL_IF_CURRENT"] = "1" if manual_update else "0"
    if update_guard_from_release is None:
        env.pop("STEVE_INSTALL_IF_LATEST", None)
        env.pop("STEVE_UPDATE_FROM_RELEASE", None)
    else:
        env["STEVE_INSTALL_IF_LATEST"] = "0" if manual_update else "1"
        env["STEVE_UPDATE_FROM_RELEASE"] = update_guard_from_release
    if old_updater_parent_pid is not None:
        env["STEVE_TEST_PARENT_PID"] = str(old_updater_parent_pid)
        env["STEVE_TEST_PARENT_START"] = process_start_time()
    result = subprocess.run(
        ["/bin/sh", str(INSTALL_SCRIPT)],
        capture_output=True,
        check=False,
        env=env,
        text=True,
    )
    requests = (
        request_log.read_text(encoding="utf-8").splitlines()
        if request_log.exists()
        else []
    )
    return result, requests


def process_start_time() -> str:
    details = subprocess.check_output(
        ["ps", "-p", str(os.getpid()), "-o", "stat=", "-o", "lstart="], text=True
    ).strip()
    return details.split(maxsplit=1)[1]


def create_package_release(
    root: Path,
    *,
    metadata_version: str = VERSION,
) -> tuple[Path, Path, str]:
    package_dir = root / "package"
    (package_dir / "bin").mkdir(parents=True)
    (package_dir / "steve-path").mkdir()
    (package_dir / "steve-package.json").write_text("{}\n", encoding="utf-8")
    write_executable(
        package_dir / "bin" / "steve",
        f"#!/bin/sh\nprintf 'steve-cli {VERSION}\\n'\n",
    )
    write_executable(
        package_dir / "bin" / "steve-code-mode-host",
        "#!/bin/sh\nexit 0\n",
    )
    write_executable(package_dir / "steve-path" / "rg", "#!/bin/sh\nexit 0\n")

    asset = "steve-package-aarch64-apple-darwin.tar.gz"
    archive_path = root / asset
    with tarfile.open(archive_path, "w:gz") as archive:
        for path in package_dir.iterdir():
            archive.add(path, arcname=path.name)

    archive_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
    checksum_path = root / "steve-package_SHA256SUMS"
    checksum_path.write_text(f"{archive_digest}  {asset}\n", encoding="utf-8")
    checksum_digest = hashlib.sha256(checksum_path.read_bytes()).hexdigest()
    metadata_json = json.dumps(
        {
            "assets": [
                {"name": asset, "digest": f"sha256:{archive_digest}"},
                {
                    "name": "steve-package_SHA256SUMS",
                    "digest": f"sha256:{checksum_digest}",
                },
            ],
            "tag_name": f"rust-v{metadata_version}",
        },
        indent=2,
    )
    return archive_path, checksum_path, metadata_json


def create_legacy_release(root: Path) -> tuple[Path, str]:
    package_dir = root / "legacy-package"
    vendor_dir = package_dir / "package" / "vendor" / "aarch64-apple-darwin"
    (vendor_dir / "steve").mkdir(parents=True)
    (vendor_dir / "path").mkdir()
    write_executable(
        vendor_dir / "steve" / "steve",
        f"#!/bin/sh\nprintf 'steve-cli {VERSION}\\n'\n",
    )
    write_executable(vendor_dir / "path" / "rg", "#!/bin/sh\nexit 0\n")

    asset = f"steve-npm-darwin-arm64-{VERSION}.tgz"
    archive_path = root / asset
    with tarfile.open(archive_path, "w:gz") as archive:
        archive.add(package_dir / "package", arcname="package")

    archive_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
    metadata_json = json.dumps(
        {
            "assets": [{"name": asset, "digest": f"sha256:{archive_digest}"}],
            "tag_name": f"rust-v{VERSION}",
        },
        indent=2,
    )
    return archive_path, metadata_json


def write_executable(path: Path, contents: str) -> None:
    path.write_text(contents, encoding="utf-8")
    path.chmod(0o755)


def release_metadata(*, compact: bool = False, reorder: bool = False) -> str:
    assets = [
        asset_metadata(
            f"steve-package-{target}.tar.gz",
            f"sha256:{'a' * 64}",
            reorder=reorder,
        )
        for target in (
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-musl",
            "x86_64-unknown-linux-musl",
        )
    ]
    assets.append(
        asset_metadata(
            "steve-package_SHA256SUMS",
            f"sha256:{'b' * 64}",
            reorder=reorder,
        )
    )
    separators = (",", ":") if compact else None
    return json.dumps(
        {"assets": assets, "body": "braces: { } [ ]", "tag_name": f"rust-v{VERSION}"},
        indent=None if compact else 2,
        separators=separators,
    )


def asset_metadata(name: str, digest: str, *, reorder: bool) -> dict[str, str]:
    if reorder:
        return {"digest": digest, "name": name}
    return {"name": name, "digest": digest}


def legacy_release_metadata_with_decoys() -> str:
    fake_digest = f"sha256:{'0' * 64}"
    assets = [
        {
            "metadata": {
                "name": "steve-package-x86_64-unknown-linux-musl.tar.gz",
                "digest": fake_digest,
            },
            "digest": f"sha256:{'c' * 64}",
            "name": f"steve-npm-{target}-{VERSION}.tgz",
        }
        for target in ("darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64")
    ]
    return json.dumps(
        {
            "body": (
                f'fake: {{"name":"steve-package_SHA256SUMS","digest":"{fake_digest}"}}'
            ),
            "assets": assets,
            "tag_name": f"rust-v{VERSION}",
        },
        separators=(",", ":"),
    )


if __name__ == "__main__":
    unittest.main()
