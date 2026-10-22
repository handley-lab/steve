//! Resolve the cyber treatment for an exec turn from the connected server's catalog.

use crate::RequestIdSequencer;
use codex_app_server_client::InProcessAppServerClient;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::CyberAccessProgram;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_protocol::openai_models::ModelAccessPrograms;

pub(crate) async fn program_for_turn(
    client: &InProcessAppServerClient,
    request_ids: &mut RequestIdSequencer,
    thread_id: &str,
    model: &str,
    enabled: bool,
) -> anyhow::Result<Option<CyberAccessProgram>> {
    let mut cursor = None;
    loop {
        let result = client
            .request_typed::<ModelListResponse>(ClientRequest::ModelList {
                request_id: request_ids.next(),
                params: ModelListParams {
                    thread_id: Some(thread_id.to_string()),
                    cursor,
                    limit: None,
                    include_hidden: Some(true),
                },
            })
            .await?;
        if let Some(selected) = result.data.into_iter().find(|entry| entry.model == model) {
            let programs = selected
                .available_access_programs
                .map(ModelAccessPrograms::from)
                .filter(|programs| !programs.cyber.is_empty());
            let Some(programs) = programs else {
                return Ok(None);
            };
            let program = if enabled {
                programs.daybreak().ok_or_else(|| anyhow::anyhow!("Daybreak is unavailable for model {model}; turn it off or choose a compatible model"))?
            } else {
                let Some(standard) = programs.standard() else {
                    return Ok(None);
                };
                standard
            };
            let forwarding_enabled = result.cyber_access_programs_enabled.ok_or_else(|| {
                anyhow::anyhow!(
                    "Cyber access program forwarding could not be determined for this thread."
                )
            })?;
            if !enabled && !forwarding_enabled {
                return Ok(None);
            }
            codex_core::cyber_access_program::for_request(Some(program), forwarding_enabled)?;
            return Ok(Some(program.into()));
        }
        match result.next_cursor {
            Some(next) => cursor = Some(next),
            None => {
                // Preserve automatic treatment for explicit model IDs absent from the catalog.
                return Ok(None);
            }
        }
    }
}
