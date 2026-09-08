//! Transport contracts for session-scoped computer use.
//! Session identity and grants come from the host, never from these arguments.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// One identified operation. Reuse the id only to recover the same operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUseCall {
    pub request_id: Uuid,
    pub name: String,
    pub arguments: Value,
}

/// Whether the host can establish that the operation took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseOutcome {
    Completed,
    Rejected,
    /// Inspect the target before proposing another action. Do not replay it.
    Unknown,
}

/// Image transport data. Agent adapters emit pixels as images, never as text.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUseImage {
    pub mime_type: String,
    pub base64: String,
}

/// A bounded operation result with images separated from model-facing text.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUseResult {
    pub request_id: Uuid,
    pub outcome: ComputerUseOutcome,
    pub text: String,
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ComputerUseImage>,
}
