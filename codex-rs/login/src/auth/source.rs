//! Compare actor credential sources for local runtime reuse, not server authentication.
//!
//! OAuth session claims are only continuity hints. The registration service verifies
//! the credential and binds the runtime; these values never grant authority locally.

use super::CodexAuth;

pub(super) fn same_source(previous: Option<&CodexAuth>, current: Option<&CodexAuth>) -> bool {
    let (Some(previous), Some(current)) = (previous, current) else {
        return false;
    };
    if previous.api_auth_mode() != current.api_auth_mode() {
        return false;
    }
    let (Ok(previous_token), Ok(current_token)) = (previous.get_token(), current.get_token())
    else {
        return false;
    };
    previous_token == current_token
}
