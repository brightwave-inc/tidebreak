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

/// Foreground-only tool names for inspecting folders the user already connected.
///
/// These contracts are model proposals, not host authority. The native executor
/// resolves the conversation from durable state and reauthorizes every broker
/// operation against that context.
pub const LIST_CONNECTED_FOLDERS_TOOL: &str = "list_connected_folders";
pub const LIST_FOLDER_TOOL: &str = "list_folder";
pub const READ_CONNECTED_FILE_TOOL: &str = "read_connected_file";

/// Maximum UTF-8 bytes in a root-relative path supplied by the model.
pub const MAX_CONNECTED_FOLDER_PATH_BYTES: usize = 1_024;

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

/// Model-facing result returned after the trusted desktop handles consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestFolderAccessResult {
    /// The user selected a folder and the broker reports live read access.
    Connected {
        /// Opaque broker-local root identity, never a host path.
        root_id: uuid::Uuid,
        /// Bounded leaf name safe for display.
        display_name: String,
        /// Capabilities the trusted host actually granted.
        capabilities: Vec<RequestedFolderCapability>,
    },
    /// The user declined or closed the picker; no access was granted.
    Declined,
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
            && !contains_absolute_path_like_text(&self.reason)
            && self.requested_capabilities == [RequestedFolderCapability::ReadFiles]
    }
}

fn contains_absolute_path_like_text(value: &str) -> bool {
    value.split_whitespace().any(|word| {
        word.starts_with('/')
            || word.starts_with("~/")
            || word.starts_with("\\\\")
            || (word.len() >= 3
                && word.as_bytes()[0].is_ascii_alphabetic()
                && word.as_bytes()[1] == b':'
                && matches!(word.as_bytes()[2], b'/' | b'\\'))
    })
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

/// Sandbox-only contract for proposing that the foreground parent consider
/// the normal folder-consent flow. It never opens a picker or grants access.
#[must_use]
pub fn sandbox_folder_access_proposal_tool_spec() -> ToolSpec {
    let mut tool = request_folder_access_tool_spec();
    tool.description = "Ask the foreground parent to decide whether it should ask the user to connect a folder. This is only a proposal: it grants no access, opens no picker, and must not include a path, root ID, or grant data.".into();
    tool
}

/// Canonical arguments for [`LIST_CONNECTED_FOLDERS_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListConnectedFoldersArgs {}

/// Canonical arguments for [`LIST_FOLDER_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListFolderArgs {
    /// Opaque id returned by `list_connected_folders` or folder consent.
    pub root_id: uuid::Uuid,
    /// Root-relative directory path; an empty path means the connected root.
    pub path: String,
}

/// Canonical arguments for [`READ_CONNECTED_FILE_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadConnectedFileArgs {
    /// Opaque id returned by `list_connected_folders` or folder consent.
    pub root_id: uuid::Uuid,
    /// Nonempty root-relative file path.
    pub path: String,
}

impl ListFolderArgs {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.root_id.is_nil() && valid_connected_folder_path(&self.path, true)
    }
}

impl ReadConnectedFileArgs {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.root_id.is_nil() && valid_connected_folder_path(&self.path, false)
    }
}

/// Validate a canonical model payload before it is durably checkpointed.
#[must_use]
pub fn validate_list_connected_folders_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<ListConnectedFoldersArgs>(arguments.clone()).is_ok()
}

/// Validate a canonical model payload before it is durably checkpointed.
#[must_use]
pub fn validate_list_folder_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<ListFolderArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

/// Validate a canonical model payload before it is durably checkpointed.
#[must_use]
pub fn validate_read_connected_file_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<ReadConnectedFileArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

#[must_use]
pub fn list_connected_folders_tool_spec() -> ToolSpec {
    ToolSpec {
        name: LIST_CONNECTED_FOLDERS_TOOL.into(),
        description: "List folders already connected to this conversation. Results contain opaque root IDs and display names only; use request_folder_access to ask the user to choose another folder.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

#[must_use]
pub fn list_folder_tool_spec() -> ToolSpec {
    ToolSpec {
        name: LIST_FOLDER_TOOL.into(),
        description: "List a directory below an already connected folder. Use only an opaque root_id and a root-relative path; never use an absolute path or parent traversal.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "root_id": { "type": "string", "format": "uuid" },
                "path": { "type": "string", "maxLength": MAX_CONNECTED_FOLDER_PATH_BYTES, "description": "Root-relative directory path; empty means the folder root." }
            },
            "required": ["root_id", "path"],
            "additionalProperties": false
        }),
    }
}

#[must_use]
pub fn read_connected_file_tool_spec() -> ToolSpec {
    ToolSpec {
        name: READ_CONNECTED_FILE_TOOL.into(),
        description: "Read a UTF-8 text file below an already connected folder. Use only an opaque root_id and a nonempty root-relative path; never use an absolute path or parent traversal.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "root_id": { "type": "string", "format": "uuid" },
                "path": { "type": "string", "minLength": 1, "maxLength": MAX_CONNECTED_FOLDER_PATH_BYTES, "description": "Root-relative text file path." }
            },
            "required": ["root_id", "path"],
            "additionalProperties": false
        }),
    }
}

fn valid_connected_folder_path(path: &str, allow_root: bool) -> bool {
    if path.len() > MAX_CONNECTED_FOLDER_PATH_BYTES
        || path.contains('\0')
        || path.contains('\\')
        || path.starts_with('/')
        || path.ends_with('/')
    {
        return false;
    }
    if path.is_empty() {
        return allow_root;
    }
    path.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.contains(':')
            && !part.chars().any(char::is_control)
    })
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
        invalid = RequestFolderAccessArgs {
            reason: "Read /Users/example/Documents".into(),
            requested_capabilities: vec![RequestedFolderCapability::ReadFiles],
            folder_hint: None,
        };
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

    #[test]
    fn folder_access_results_never_need_a_host_path() {
        let result = RequestFolderAccessResult::Connected {
            root_id: uuid::Uuid::new_v4(),
            display_name: "Documents".into(),
            capabilities: vec![RequestedFolderCapability::ReadFiles],
        };
        let encoded = serde_json::to_value(&result).unwrap();
        assert_eq!(encoded["status"], "connected");
        assert!(encoded.get("path").is_none());
        assert_eq!(
            serde_json::from_value::<RequestFolderAccessResult>(encoded).unwrap(),
            result
        );
        assert_eq!(
            serde_json::to_value(RequestFolderAccessResult::Declined).unwrap(),
            serde_json::json!({ "status": "declined" })
        );
    }

    #[test]
    fn connected_folder_tools_are_pathless_and_conservatively_bounded() {
        assert!(validate_list_connected_folders_arguments(
            &serde_json::json!({})
        ));
        assert!(!validate_list_connected_folders_arguments(
            &serde_json::json!({
                "root_id": uuid::Uuid::new_v4()
            })
        ));

        let root_id = uuid::Uuid::new_v4();
        assert!(validate_list_folder_arguments(&serde_json::json!({
            "root_id": root_id,
            "path": ""
        })));
        assert!(validate_read_connected_file_arguments(&serde_json::json!({
            "root_id": root_id,
            "path": "notes/today.txt"
        })));
        for path in [
            "/tmp/secret",
            "../secret",
            "notes/../secret",
            "notes\\secret",
            "C:secret",
        ] {
            assert!(!validate_read_connected_file_arguments(
                &serde_json::json!({
                    "root_id": root_id,
                    "path": path
                })
            ));
        }
        assert!(!validate_read_connected_file_arguments(
            &serde_json::json!({
                "root_id": root_id,
                "path": ""
            })
        ));
    }

    #[test]
    fn connected_folder_specs_do_not_advertise_host_paths_or_authority() {
        let list = list_connected_folders_tool_spec();
        let directory = list_folder_tool_spec();
        let file = read_connected_file_tool_spec();
        assert_eq!(list.name, LIST_CONNECTED_FOLDERS_TOOL);
        assert_eq!(directory.name, LIST_FOLDER_TOOL);
        assert_eq!(file.name, READ_CONNECTED_FILE_TOOL);
        assert_eq!(directory.input_schema["additionalProperties"], false);
        assert_eq!(file.input_schema["additionalProperties"], false);
        assert!(!directory.description.contains("project_id"));
        assert!(!file.description.contains("grant"));
    }
}
