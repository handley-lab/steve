//! Forward explicit programs while leaving entitlement and model policy to the server.

use codex_api::AccessPrograms;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::turn_input::CyberAccessProgram;

pub fn for_request(
    program: Option<CyberAccessProgram>,
    enabled: bool,
) -> Result<Option<AccessPrograms>> {
    let Some(program) = program else {
        return Ok(None);
    };
    if !enabled {
        return Err(CodexErr::InvalidRequest(
            "Cyber access programs are disabled for this session.".to_owned(),
        ));
    }
    Ok(Some(program.into()))
}
