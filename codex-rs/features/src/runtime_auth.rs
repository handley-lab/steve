//! Authentication modes for runtime assertions.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAuthMode {
    /// Send the actor credential and a runtime assertion together during migration.
    #[default]
    DualHeader,
    /// Send only the runtime assertion as the request credential.
    AssertionOnly,
}
