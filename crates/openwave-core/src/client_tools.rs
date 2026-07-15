//! Shared contracts for work that must execute on a trusted client.
//!
//! These types describe model proposals, not authority. A desktop or other
//! trusted client must still validate the payload, obtain explicit user
//! consent, and derive the actual host-broker grant itself.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::ToolSpec;

/// Stable tool name for asking the user to connect another folder.
pub const REQUEST_FOLDER_ACCESS_TOOL: &str = "request_folder_access";

/// Maximum user-facing explanation length advertised to the model.
pub const MAX_FOLDER_ACCESS_REASON_CHARS: usize = 500;

/// One untrusted capability proposal attached to a folder-access request.
///
/// The trusted host derives the granted capabilities after consent; it must not
/// copy this list directly into a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestedFolderCapability {
    /// List directories and read files below the selected folder.
    ReadFiles,
}

/// Non-authoritative, well-known starting location for the native picker.
///
/// This is deliberately not a free-form path. The trusted desktop decides how
/// (or whether) to map it to a local picker location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestedFolderHint {
    /// Start the picker near the user's documents folder when supported.
    Documents,
    /// Start the picker near the user's downloads folder when supported.
    Downloads,
}

/// Canonical arguments for [`REQUEST_FOLDER_ACCESS_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFolderAccessArgs {
    /// Short explanation shown to the user before any picker opens.
    pub reason: String,
    /// Capabilities the model believes it needs; these are proposals only.
    pub requested_capabilities: Vec<RequestedFolderCapability>,
    /// Optional non-authoritative well-known picker hint.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_hint"
    )]
    pub folder_hint: Option<RequestedFolderHint>,
}

fn deserialize_optional_hint<'de, D>(
    deserializer: D,
) -> Result<Option<RequestedFolderHint>, D::Error>
where
    D: Deserializer<'de>,
{
    RequestedFolderHint::deserialize(deserializer).map(Some)
}

impl RequestFolderAccessArgs {
    /// Whether a trusted client may safely present this proposal to the user.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.reason.trim().is_empty()
            && self.reason.chars().count() <= MAX_FOLDER_ACCESS_REASON_CHARS
            && !self.reason.contains('\0')
            && self.requested_capabilities == [RequestedFolderCapability::ReadFiles]
    }
}

/// Validate one canonical JSON payload before it crosses the trusted-client boundary.
#[must_use]
pub fn validate_request_folder_access_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<RequestFolderAccessArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

/// Tool contract advertised by the local control plane.
#[must_use]
pub fn request_folder_access_tool_spec() -> ToolSpec {
    ToolSpec {
        name: REQUEST_FOLDER_ACCESS_TOOL.into(),
        description: "Ask the user to connect another folder through a native consent flow. This request grants no access by itself. Provide a short reason, the read capability proposal, and optionally a label such as Documents or Downloads; never provide an absolute path.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_FOLDER_ACCESS_REASON_CHARS,
                    "description": "Why access is needed, shown to the user."
                },
                "requested_capabilities": {
                    "type": "array",
                    "description": "Untrusted capability proposals; the host derives any actual grant.",
                    "items": { "enum": ["read_files"] },
                    "minItems": 1,
                    "maxItems": 1,
                    "uniqueItems": true
                },
                "folder_hint": {
                    "enum": ["documents", "downloads"],
                    "description": "Optional well-known picker hint, never an absolute path."
                }
            },
            "required": ["reason", "requested_capabilities"],
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_access_contract_is_bounded_and_denies_unknown_fields() {
        let args: RequestFolderAccessArgs = serde_json::from_value(serde_json::json!({
            "reason": "Read the reports needed for this project",
            "requested_capabilities": ["read_files"],
            "folder_hint": "documents"
        }))
        .unwrap();
        assert!(args.is_well_formed());
        assert!(validate_request_folder_access_arguments(
            &serde_json::to_value(&args).unwrap()
        ));
        assert!(
            serde_json::from_value::<RequestFolderAccessArgs>(serde_json::json!({
                "reason": "Read reports",
                "requested_capabilities": ["read_files"],
                "path": "/Users/example/Documents"
            }))
            .is_err()
        );

        let mut invalid = args.clone();
        invalid.reason = " ".into();
        assert!(!invalid.is_well_formed());
        invalid = args.clone();
        invalid.requested_capabilities.clear();
        assert!(!invalid.is_well_formed());
        invalid = args;
        invalid.reason = format!("{}x", " ".repeat(MAX_FOLDER_ACCESS_REASON_CHARS));
        assert!(!invalid.is_well_formed());
        assert!(
            serde_json::from_value::<RequestFolderAccessArgs>(serde_json::json!({
                "reason": "Read reports",
                "requested_capabilities": ["read_files"],
                "folder_hint": "/Users/example/Documents"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RequestFolderAccessArgs>(serde_json::json!({
                "reason": "Read reports",
                "requested_capabilities": ["read_files"],
                "folder_hint": null
            }))
            .is_err()
        );
    }

    #[test]
    fn folder_access_spec_marks_every_proposal_as_non_authoritative() {
        let spec = request_folder_access_tool_spec();
        assert_eq!(spec.name, REQUEST_FOLDER_ACCESS_TOOL);
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert_eq!(
            spec.input_schema["required"],
            serde_json::json!(["reason", "requested_capabilities"])
        );
        assert!(spec.description.contains("grants no access"));
    }
}
