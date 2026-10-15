use std::ffi::OsString;

use codex_protocol::shell_environment::OPENAI_FEDERATION_RULE_ID_ENV_VAR;
use codex_protocol::shell_environment::OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR;

/// Returns whether workload identity was selected through process configuration.
///
/// Either marker selects workload identity. This reads the markers only.
pub fn is_workload_identity_selected() -> bool {
    ProcessEnvironment::read().has_marker()
}

struct ProcessEnvironment {
    federation_rule_id: Option<OsString>,
    identity_token_file: Option<OsString>,
}

impl ProcessEnvironment {
    fn read() -> Self {
        Self {
            federation_rule_id: std::env::var_os(OPENAI_FEDERATION_RULE_ID_ENV_VAR),
            identity_token_file: std::env::var_os(OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR),
        }
    }

    fn has_marker(&self) -> bool {
        self.federation_rule_id.is_some() || self.identity_token_file.is_some()
    }
}
