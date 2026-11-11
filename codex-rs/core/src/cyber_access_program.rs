//! Forward explicit programs while leaving entitlement and model policy to the server.

use codex_api::AccessPrograms;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::turn_input::CyberAccessProgram;
use std::sync::Arc;

/// Host requirement for every model request issued by this thread.
///
/// Install in thread extension data before startup and reapply on cold resume.
/// Core snapshots it before extension callbacks; it is not inherited or persisted.
/// Every request must resolve to one listed program; backend authorization remains authoritative.
#[derive(Clone, Debug)]
pub struct RequiredCyberAccessPrograms {
    pub allowed: Vec<CyberAccessProgram>,
}

pub(crate) struct RequiredCyberAccessProgramsSnapshot(pub Arc<RequiredCyberAccessPrograms>);

impl RequiredCyberAccessPrograms {
    /// Read Core's startup snapshot, independent of later edits to the public attachment.
    pub fn for_thread(data: &codex_extension_api::ExtensionData) -> Option<Arc<Self>> {
        data.get::<RequiredCyberAccessProgramsSnapshot>()
            .map(|snapshot| Arc::clone(&snapshot.0))
    }
}

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
