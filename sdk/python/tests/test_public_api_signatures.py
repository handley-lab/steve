from __future__ import annotations

import importlib.resources as resources
import inspect
from pathlib import Path
from typing import Any

import steve_sdk
import steve_sdk.types as public_types
from steve_sdk import (
    ApprovalMode,
    AsyncSteve,
    AsyncThread,
    AsyncTurnHandle,
    ExternalMessage,
    Sandbox,
    Steve,
    SteveConfig,
    Thread,
    TurnHandle,
    TurnResult,
)
from steve_sdk._initialize_metadata import validate_initialize_metadata
from steve_sdk.types import InitializeResponse

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib

EXPECTED_ROOT_EXPORTS = [
    "__version__",
    "SteveConfig",
    "Steve",
    "AsyncSteve",
    "ApprovalMode",
    "Sandbox",
    "ChatgptLoginHandle",
    "DeviceCodeLoginHandle",
    "AsyncChatgptLoginHandle",
    "AsyncDeviceCodeLoginHandle",
    "Thread",
    "AsyncThread",
    "TurnHandle",
    "AsyncTurnHandle",
    "TurnResult",
    "Input",
    "InputItem",
    "RunInput",
    "ExternalMessage",
    "TextInput",
    "ImageInput",
    "LocalImageInput",
    "SkillInput",
    "MentionInput",
    "retry_on_overload",
    "SteveError",
    "TransportClosedError",
    "JsonRpcError",
    "SteveRpcError",
    "ParseError",
    "InvalidRequestError",
    "MethodNotFoundError",
    "InvalidParamsError",
    "InternalRpcError",
    "ServerBusyError",
    "RetryLimitExceededError",
    "is_retryable_error",
]

EXPECTED_TYPES_EXPORTS = [
    "Account",
    "AccountLoginCompletedNotification",
    "ApprovalsReviewer",
    "AskForApproval",
    "CancelLoginAccountResponse",
    "CancelLoginAccountStatus",
    "GetAccountResponse",
    "InitializeResponse",
    "JsonObject",
    "ModelListResponse",
    "Notification",
    "Personality",
    "PlanType",
    "ReasoningEffort",
    "ReasoningSummary",
    "SandboxMode",
    "SandboxPolicy",
    "SortDirection",
    "ThreadArchiveResponse",
    "ThreadCompactStartResponse",
    "ThreadItem",
    "ThreadListCwdFilter",
    "ThreadListResponse",
    "ThreadReadResponse",
    "ThreadSetNameResponse",
    "ThreadSortKey",
    "ThreadSource",
    "ThreadSourceKind",
    "ThreadStartSource",
    "ThreadTokenUsage",
    "ThreadTokenUsageUpdatedNotification",
    "Turn",
    "TurnCompletedNotification",
    "TurnError",
    "TurnInterruptResponse",
    "TurnStatus",
    "TurnSteerResponse",
]


def _keyword_only_names(fn: object) -> list[str]:
    """Return only user-facing keyword-only parameter names for a public method."""
    signature = inspect.signature(fn)
    return [
        param.name
        for param in signature.parameters.values()
        if param.kind == inspect.Parameter.KEYWORD_ONLY
    ]


def _keyword_default(fn: object, name: str) -> object:
    """Return the default value for one keyword parameter on a public method."""
    return inspect.signature(fn).parameters[name].default


def _assert_no_any_annotations(fn: object) -> None:
    """Reject loose annotations on public wrapper methods."""
    signature = inspect.signature(fn)
    for param in signature.parameters.values():
        if param.annotation is Any:
            raise AssertionError(f"{fn} has public parameter typed as Any: {param.name}")
    if signature.return_annotation is Any:
        raise AssertionError(f"{fn} has public return annotation typed as Any")


def test_root_exports_codex_config() -> None:
    """The root package should expose the process configuration object."""
    assert SteveConfig.__name__ == "SteveConfig"


def test_root_exports_turn_result() -> None:
    """The root package should expose the collected turn result wrapper."""
    assert {
        "name": TurnResult.__name__,
        "fields": list(TurnResult.__dataclass_fields__),
    } == {
        "name": "TurnResult",
        "fields": [
            "id",
            "status",
            "error",
            "started_at",
            "completed_at",
            "duration_ms",
            "final_response",
            "items",
            "usage",
        ],
    }


def test_turn_run_methods_return_turn_result() -> None:
    """Both convenience and handle-based run APIs return the same result shape."""
    funcs = [
        Thread.run,
        TurnHandle.run,
        AsyncThread.run,
        AsyncTurnHandle.run,
    ]

    assert {fn: inspect.signature(fn).return_annotation for fn in funcs} == dict.fromkeys(
        funcs, "TurnResult"
    )


def test_turn_input_methods_accept_string_shortcut() -> None:
    """Every public turn-input method should accept strings and typed inputs."""
    funcs = [
        Thread.run,
        Thread.turn,
        AsyncThread.run,
        AsyncThread.turn,
    ]

    assert {fn: inspect.signature(fn).parameters["input"].annotation for fn in funcs} == (
        dict.fromkeys(funcs, "RunInput")
    )
    assert {
        fn: inspect.signature(fn).parameters["input"].annotation
        for fn in (TurnHandle.steer, AsyncTurnHandle.steer)
    } == dict.fromkeys((TurnHandle.steer, AsyncTurnHandle.steer), "Input | str")


def test_root_exports_approval_mode() -> None:
    """The root package should expose the high-level approval mode enum."""
    assert [(mode.name, mode.value) for mode in ApprovalMode] == [
        ("deny_all", "deny_all"),
        ("auto_review", "auto_review"),
    ]


def test_root_exports_sandbox_presets() -> None:
    """The friendly sandbox API should expose only obvious named presets."""
    assert [(sandbox.name, sandbox.value) for sandbox in Sandbox] == [
        ("read_only", "read-only"),
        ("workspace_write", "workspace-write"),
        ("full_access", "full-access"),
    ]


def test_package_and_default_client_versions_follow_project_version() -> None:
    """The importable package version should stay aligned with pyproject metadata."""
    pyproject_path = Path(__file__).resolve().parents[1] / "pyproject.toml"
    pyproject = tomllib.loads(pyproject_path.read_text())

    assert steve_sdk.__version__ == pyproject["project"]["version"]
    assert SteveConfig().client_version == steve_sdk.__version__


def test_curated_public_api_has_builtin_help_documentation() -> None:
    """The package's normal ``help()`` surface should explain common first-use APIs."""
    documented = {
        "module": steve_sdk,
        "Steve": Steve,
        "AsyncSteve": AsyncSteve,
        "SteveConfig": SteveConfig,
        "Thread": Thread,
        "AsyncThread": AsyncThread,
        "TurnHandle": TurnHandle,
        "AsyncTurnHandle": AsyncTurnHandle,
        "TurnResult": TurnResult,
        "ExternalMessage": ExternalMessage,
        "Sandbox": Sandbox,
        "thread_start": Steve.thread_start,
        "thread_resume": Steve.thread_resume,
        "thread_run": Thread.run,
        "thread_turn": Thread.turn,
    }

    assert {name: inspect.getdoc(value) is not None for name, value in documented.items()} == (
        dict.fromkeys(documented, True)
    )


def test_package_includes_py_typed_marker() -> None:
    """The wheel should advertise that inline type information is available."""
    marker = resources.files("steve_sdk").joinpath("py.typed")
    assert marker.is_file()


def test_package_root_exports_only_public_api() -> None:
    """The package root should expose the supported SDK surface, not internals."""
    assert steve_sdk.__all__ == EXPECTED_ROOT_EXPORTS
    assert {name: hasattr(steve_sdk, name) for name in EXPECTED_ROOT_EXPORTS} == dict.fromkeys(
        EXPECTED_ROOT_EXPORTS, True
    )
    assert {
        "CodexClient": hasattr(steve_sdk, "CodexClient"),
        "AsyncCodexClient": hasattr(steve_sdk, "AsyncCodexClient"),
        "InitializeResponse": hasattr(steve_sdk, "InitializeResponse"),
        "ThreadStartParams": hasattr(steve_sdk, "ThreadStartParams"),
        "TurnStartParams": hasattr(steve_sdk, "TurnStartParams"),
        "TurnCompletedNotification": hasattr(steve_sdk, "TurnCompletedNotification"),
        "TurnStatus": hasattr(steve_sdk, "TurnStatus"),
    } == {
        "CodexClient": False,
        "AsyncCodexClient": False,
        "InitializeResponse": False,
        "ThreadStartParams": False,
        "TurnStartParams": False,
        "TurnCompletedNotification": False,
        "TurnStatus": False,
    }


def test_package_star_import_matches_public_api() -> None:
    """Star imports should follow the same explicit public API list."""
    namespace: dict[str, object] = {}
    exec("from steve_sdk import *", namespace)

    exported = set(namespace) - {"__builtins__"}
    assert exported == set(EXPECTED_ROOT_EXPORTS)


def test_types_module_exports_curated_public_types() -> None:
    """The public type module should expose Steve protocol models."""
    assert public_types.__all__ == EXPECTED_TYPES_EXPORTS
    assert {name: hasattr(public_types, name) for name in EXPECTED_TYPES_EXPORTS} == dict.fromkeys(
        EXPECTED_TYPES_EXPORTS, True
    )


def test_types_star_import_matches_public_types() -> None:
    """Star imports from the type module should match its explicit export list."""
    namespace: dict[str, object] = {}
    exec("from steve_sdk.types import *", namespace)

    exported = set(namespace) - {"__builtins__"}
    assert exported == set(EXPECTED_TYPES_EXPORTS)


def test_examples_use_public_import_surfaces() -> None:
    """Examples should teach users the public root and type-module imports only."""
    examples_root = Path(__file__).resolve().parents[1] / "examples"
    private_import_markers = [
        "steve_sdk.api",
        "steve_sdk.client",
        "steve_sdk.generated",
        "steve_sdk.models",
        "steve_sdk.retry",
    ]

    offenders = {
        str(path.relative_to(examples_root)): marker
        for path in examples_root.rglob("*.py")
        for marker in private_import_markers
        if marker in path.read_text()
    }

    assert offenders == {}


def test_generated_public_signatures_are_snake_case_and_typed() -> None:
    """Generated convenience methods should expose typed Pythonic keyword names."""
    expected = {
        Steve.thread_start: [
            "approval_mode",
            "base_instructions",
            "config",
            "cwd",
            "developer_instructions",
            "ephemeral",
            "model",
            "model_provider",
            "personality",
            "sandbox",
            "service_name",
            "service_tier",
            "session_start_source",
            "thread_source",
        ],
        Steve.thread_list: [
            "archived",
            "cursor",
            "cwd",
            "limit",
            "model_providers",
            "search_term",
            "section_id",
            "sort_direction",
            "sort_key",
            "source_kinds",
            "use_state_db_only",
        ],
        Steve.thread_resume: [
            "approval_mode",
            "base_instructions",
            "config",
            "cwd",
            "developer_instructions",
            "include_turns",
            "model",
            "model_provider",
            "personality",
            "sandbox",
            "service_tier",
        ],
        Steve.thread_fork: [
            "approval_mode",
            "base_instructions",
            "config",
            "cwd",
            "developer_instructions",
            "ephemeral",
            "include_turns",
            "model",
            "model_provider",
            "sandbox",
            "service_tier",
            "thread_source",
        ],
        Thread.turn: [
            "approval_mode",
            "cwd",
            "effort",
            "model",
            "output_schema",
            "personality",
            "sandbox",
            "service_tier",
            "source",
            "summary",
            "turn_service_tier",
        ],
        Thread.run: [
            "approval_mode",
            "cwd",
            "effort",
            "model",
            "output_schema",
            "personality",
            "sandbox",
            "service_tier",
            "source",
            "summary",
            "turn_service_tier",
        ],
        AsyncSteve.thread_start: [
            "approval_mode",
            "base_instructions",
            "config",
            "cwd",
            "developer_instructions",
            "ephemeral",
            "model",
            "model_provider",
            "personality",
            "sandbox",
            "service_name",
            "service_tier",
            "session_start_source",
            "thread_source",
        ],
        AsyncSteve.thread_list: [
            "archived",
            "cursor",
            "cwd",
            "limit",
            "model_providers",
            "search_term",
            "section_id",
            "sort_direction",
            "sort_key",
            "source_kinds",
            "use_state_db_only",
        ],
        AsyncSteve.thread_resume: [
            "approval_mode",
            "base_instructions",
            "config",
            "cwd",
            "developer_instructions",
            "include_turns",
            "model",
            "model_provider",
            "personality",
            "sandbox",
            "service_tier",
        ],
        AsyncSteve.thread_fork: [
            "approval_mode",
            "base_instructions",
            "config",
            "cwd",
            "developer_instructions",
            "ephemeral",
            "include_turns",
            "model",
            "model_provider",
            "sandbox",
            "service_tier",
            "thread_source",
        ],
        AsyncThread.turn: [
            "approval_mode",
            "cwd",
            "effort",
            "model",
            "output_schema",
            "personality",
            "sandbox",
            "service_tier",
            "source",
            "summary",
            "turn_service_tier",
        ],
        AsyncThread.run: [
            "approval_mode",
            "cwd",
            "effort",
            "model",
            "output_schema",
            "personality",
            "sandbox",
            "service_tier",
            "source",
            "summary",
            "turn_service_tier",
        ],
    }

    for fn, expected_kwargs in expected.items():
        actual = _keyword_only_names(fn)
        assert actual == expected_kwargs, f"unexpected kwargs for {fn}: {actual}"
        assert all(name == name.lower() for name in actual), (
            f"non snake_case kwargs in {fn}: {actual}"
        )
        _assert_no_any_annotations(fn)


def test_new_thread_methods_default_to_auto_review() -> None:
    """New threads should start with auto-review unless callers opt out."""
    funcs = [
        Steve.thread_start,
        AsyncSteve.thread_start,
    ]

    assert {fn: _keyword_default(fn, "approval_mode") for fn in funcs} == dict.fromkeys(
        funcs, ApprovalMode.auto_review
    )


def test_existing_thread_methods_default_to_preserving_approval_settings() -> None:
    """Existing thread operations should not serialize approval overrides by default."""
    funcs = [
        Steve.thread_resume,
        Steve.thread_fork,
        Thread.turn,
        Thread.run,
        AsyncSteve.thread_resume,
        AsyncSteve.thread_fork,
        AsyncThread.turn,
        AsyncThread.run,
    ]

    assert {fn: _keyword_default(fn, "approval_mode") for fn in funcs} == dict.fromkeys(funcs)


def test_lifecycle_methods_are_codex_scoped() -> None:
    """Lifecycle operations should hang off the client rather than thread objects."""
    assert hasattr(Steve, "thread_resume")
    assert hasattr(Steve, "thread_fork")
    assert hasattr(Steve, "thread_archive")
    assert hasattr(Steve, "thread_unarchive")
    assert hasattr(AsyncSteve, "thread_resume")
    assert hasattr(AsyncSteve, "thread_fork")
    assert hasattr(AsyncSteve, "thread_archive")
    assert hasattr(AsyncSteve, "thread_unarchive")
    assert not hasattr(Steve, "thread")
    assert not hasattr(AsyncSteve, "thread")

    assert not hasattr(Thread, "resume")
    assert not hasattr(Thread, "fork")
    assert not hasattr(Thread, "archive")
    assert not hasattr(Thread, "unarchive")
    assert not hasattr(AsyncThread, "resume")
    assert not hasattr(AsyncThread, "fork")
    assert not hasattr(AsyncThread, "archive")
    assert not hasattr(AsyncThread, "unarchive")

    for fn in (
        Steve.thread_archive,
        Steve.thread_unarchive,
        AsyncSteve.thread_archive,
        AsyncSteve.thread_unarchive,
    ):
        _assert_no_any_annotations(fn)


def test_initialize_metadata_parses_user_agent_shape() -> None:
    """Initialize metadata should accept the legacy user-agent-only payload shape."""
    payload = InitializeResponse.model_validate({"userAgent": "codex-cli/1.2.3"})
    parsed = validate_initialize_metadata(payload)
    assert parsed is payload
    assert parsed.userAgent == "codex-cli/1.2.3"
    assert parsed.serverInfo is not None
    assert parsed.serverInfo.name == "codex-cli"
    assert parsed.serverInfo.version == "1.2.3"


def test_initialize_metadata_requires_non_empty_information() -> None:
    """Initialize metadata should fail when the runtime gives no identity signal."""
    try:
        validate_initialize_metadata(InitializeResponse.model_validate({}))
    except RuntimeError as exc:
        assert "missing required metadata" in str(exc)
    else:
        raise AssertionError("expected RuntimeError when initialize metadata is missing")
