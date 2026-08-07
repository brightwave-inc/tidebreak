//! Shared contracts for work that must execute on a trusted client.
//!
//! These types describe model proposals, not authority. A desktop or other
//! trusted client must still validate the payload, obtain explicit user
//! consent, and derive the actual host-broker grant itself.

use schemars::JsonSchema;
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
pub const IMPORT_CONNECTED_FILE_TOOL: &str = "import_connected_file";
/// Stable name for publishing an existing immutable output into an attached root.
pub const WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL: &str = "write_output_to_connected_folder";

/// Maximum UTF-8 bytes in a root-relative path supplied by the model.
pub const MAX_CONNECTED_FOLDER_PATH_BYTES: usize = 1_024;

/// Maximum user-facing explanation length advertised to the model.
pub const MAX_FOLDER_ACCESS_REASON_CHARS: usize = 500;

/// One untrusted capability proposal attached to a folder-access request.
///
/// The trusted host derives the granted capabilities after consent; it must not
/// copy this list directly into a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "", transform = preserve_enum_wire_shape)]
#[non_exhaustive]
pub enum RequestedFolderCapability {
    /// List directories and read files below the selected folder.
    #[schemars(description = "")]
    ReadFiles,
}

/// One folder capability the trusted host reports as currently granted.
///
/// This vocabulary is deliberately separate from [`RequestedFolderCapability`]:
/// the request remains a narrow, untrusted proposal while the result reports
/// the broker's real authorization decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GrantedFolderCapability {
    /// List directories and read files below the selected folder.
    ReadFiles,
    /// Create files or replace them after any required approval.
    WriteFiles,
    /// Expose the selected folder to model-authored commands.
    ExecuteCommands,
}

/// Non-authoritative, well-known starting location for the native picker.
///
/// This is deliberately not a free-form path. The trusted desktop decides how
/// (or whether) to map it to a local picker location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "", transform = preserve_enum_wire_shape)]
#[non_exhaustive]
pub enum RequestedFolderHint {
    /// Start the picker near the user's documents folder when supported.
    #[schemars(description = "")]
    Documents,
    /// Start the picker near the user's downloads folder when supported.
    #[schemars(description = "")]
    Downloads,
}

// Unit-variant docs make Schemars prefer `oneOf`; providers already consume
// these two contracts as compact enum-only schemas.
fn preserve_enum_wire_shape(schema: &mut schemars::Schema) {
    if let Some(Value::Array(variants)) = schema.remove("oneOf") {
        let values = variants
            .iter()
            .map(|variant| variant.get("const").cloned())
            .collect::<Option<Vec<_>>>();
        if let Some(values) = values {
            schema.insert("enum".into(), Value::Array(values));
        } else {
            schema.insert("oneOf".into(), Value::Array(variants));
        }
    }
    schema.remove("type");
}

/// Canonical arguments for [`REQUEST_FOLDER_ACCESS_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestFolderAccessArgs {
    /// Short explanation shown to the user before any picker opens.
    #[schemars(
        length(min = 1, max = MAX_FOLDER_ACCESS_REASON_CHARS),
        description = "Why access is needed, shown to the user."
    )]
    pub reason: String,
    /// Capabilities the model believes it needs; these are proposals only.
    #[schemars(
        length(min = 1, max = 1),
        extend("uniqueItems" = true),
        description = "Untrusted capability proposals; the host derives any actual grant."
    )]
    pub requested_capabilities: Vec<RequestedFolderCapability>,
    /// Optional non-authoritative well-known picker hint.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_hint"
    )]
    #[schemars(
        with = "RequestedFolderHint",
        description = "Optional well-known picker hint, never an absolute path."
    )]
    pub folder_hint: Option<RequestedFolderHint>,
}

/// Model-facing result returned after the trusted desktop handles consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestFolderAccessResult {
    /// The user selected a folder and the broker reports its live capabilities.
    Connected {
        /// Opaque broker-local root identity, never a host path.
        root_id: uuid::Uuid,
        /// Bounded leaf name safe for display.
        display_name: String,
        /// Capabilities the trusted host actually granted.
        capabilities: Vec<GrantedFolderCapability>,
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
    ToolSpec::for_args::<RequestFolderAccessArgs>(
        REQUEST_FOLDER_ACCESS_TOOL,
        "Ask the user to connect another folder through a native consent flow. This request grants no access by itself. Provide a short reason, the read capability proposal, and optionally a label such as Documents or Downloads; never provide an absolute path.",
    )
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListConnectedFoldersArgs {}

/// Canonical arguments for [`LIST_FOLDER_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListFolderArgs {
    /// Opaque id returned by `list_connected_folders` or folder consent.
    #[schemars(description = "")]
    pub root_id: uuid::Uuid,
    /// Root-relative directory path; an empty path means the connected root.
    #[schemars(
        length(max = MAX_CONNECTED_FOLDER_PATH_BYTES),
        description = "Root-relative directory path; empty means the folder root."
    )]
    pub path: String,
}

/// Canonical arguments for [`READ_CONNECTED_FILE_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadConnectedFileArgs {
    /// Opaque id returned by `list_connected_folders` or folder consent.
    #[schemars(description = "")]
    pub root_id: uuid::Uuid,
    /// Nonempty root-relative file path.
    #[schemars(
        length(min = 1, max = MAX_CONNECTED_FOLDER_PATH_BYTES),
        description = "Root-relative text file path."
    )]
    pub path: String,
}

/// Canonical arguments for [`IMPORT_CONNECTED_FILE_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportConnectedFileArgs {
    /// Opaque id returned by `list_connected_folders` or folder consent.
    #[schemars(description = "")]
    pub root_id: uuid::Uuid,
    /// Nonempty root-relative file path.
    #[schemars(
        length(min = 1, max = MAX_CONNECTED_FOLDER_PATH_BYTES),
        description = "Root-relative file path."
    )]
    pub path: String,
}

/// Whether connected-folder publication may replace an existing regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "", transform = preserve_enum_wire_shape)]
pub enum OutputWriteMode {
    /// Create the destination atomically and refuse an existing entry.
    #[schemars(description = "")]
    Create,
    /// Replace one existing regular file after a fresh native approval.
    #[schemars(description = "")]
    Replace,
}

impl OutputWriteMode {
    /// Whether publishing an output this way needs a fresh user decision in a
    /// chat running under `mode`.
    ///
    /// This is the one client-executed write that leaves the sandbox and lands
    /// in a real folder the reader owns, so it is the one client call the
    /// chat's permission mode governs. `Ask` promises that workspace mutations
    /// ask, and a create is a workspace mutation; `Auto` and `Allow` are
    /// standing answers to exactly that question and let it through. `Plan`
    /// never checkpoints the call at all, but a mode flipped while one is
    /// parked resolves the conservative way.
    ///
    /// Replacement always asks, in every mode: it destroys bytes the agent did
    /// not write, and no mode advertises consent for that.
    #[must_use]
    pub fn requires_user_decision(self, mode: Option<crate::PermissionMode>) -> bool {
        match self {
            Self::Replace => true,
            Self::Create => matches!(
                mode.unwrap_or(crate::PermissionMode::Ask),
                crate::PermissionMode::Ask | crate::PermissionMode::Plan
            ),
        }
    }
}

/// Model proposal for writing one published output to an attached root.
///
/// The model names the output by its display filename — exec results report
/// published outputs by filename only, and output ids are deliberately not
/// part of the model's vocabulary. The agent resolves the filename against
/// the conversation's live outputs and checkpoints the canonical id-bearing
/// [`WriteOutputToConnectedFolderArgs`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteOutputToConnectedFolderProposal {
    /// Display filename of a published output owned by this conversation.
    #[schemars(
        length(min = 1, max = crate::deliverable::MAX_DELIVERABLE_NAME_CHARS),
        description = "Filename of a published output of this conversation, exactly as reported when it was published."
    )]
    pub filename: String,
    /// Opaque root identity already attached to this conversation.
    #[schemars(description = "")]
    pub root_id: uuid::Uuid,
    /// Nonempty destination relative to the attached root.
    #[schemars(
        length(min = 1, max = MAX_CONNECTED_FOLDER_PATH_BYTES),
        description = "Nonempty root-relative destination path."
    )]
    pub path: String,
    /// Create/no-clobber by default; replacement requires a fresh native approval.
    #[schemars(description = "")]
    pub mode: OutputWriteMode,
}

/// Canonical checkpointed arguments for writing one existing output to an
/// attached root.
///
/// This is the durable host-side form: the agent resolves the model's
/// [`WriteOutputToConnectedFolderProposal`] filename into the opaque output
/// identity before the call is checkpointed, so everything downstream (server
/// route, approval card, native write-back) works from a stable identity.
/// Output bytes and revision identity are deliberately absent. The trusted
/// native executor resolves both from the authoritative output record at claim
/// time, then binds them into its private recovery receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteOutputToConnectedFolderArgs {
    /// Existing opaque output identity owned by this conversation.
    #[schemars(description = "")]
    pub output_id: uuid::Uuid,
    /// Opaque root identity already attached to this conversation.
    #[schemars(description = "")]
    pub root_id: uuid::Uuid,
    /// Nonempty destination relative to the attached root.
    #[schemars(
        length(min = 1, max = MAX_CONNECTED_FOLDER_PATH_BYTES),
        description = "Nonempty root-relative destination path."
    )]
    pub path: String,
    /// Create/no-clobber by default; replacement requires a fresh native approval.
    #[schemars(description = "")]
    pub mode: OutputWriteMode,
}

/// Model-facing outcome of one import proposal.
///
/// Deliberately says nothing about how the file was read. The model proposed a
/// root-relative path; the trusted client decided whether that proposal was
/// still authorized, what the bytes actually were, and what the source became.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportConnectedFileResult {
    /// The file became a source in this conversation.
    Imported {
        /// Id usable with `list_sources` and `read_source`.
        document_id: uuid::Uuid,
        /// Bounded leaf name safe for display.
        title: String,
        /// Media type the trusted client determined from the bytes.
        media_type: String,
        /// Decoded size of the imported source.
        bytes: u64,
        /// What can be done with the source now.
        readiness: crate::SourceReadiness,
    },
    /// Nothing was imported, and no host detail explains why.
    Unavailable {
        /// Short reason safe to show a user.
        message: String,
    },
}

impl ListFolderArgs {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.root_id.is_nil() && valid_connected_folder_path(&self.path, true)
    }
}

impl ImportConnectedFileArgs {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.root_id.is_nil() && valid_connected_folder_path(&self.path, false)
    }
}

impl ReadConnectedFileArgs {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.root_id.is_nil() && valid_connected_folder_path(&self.path, false)
    }
}

impl WriteOutputToConnectedFolderProposal {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        crate::deliverable::validate_portable_filename(&self.filename).is_ok()
            && !self.root_id.is_nil()
            && valid_connected_folder_path(&self.path, false)
    }
}

impl WriteOutputToConnectedFolderArgs {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.output_id.is_nil()
            && !self.root_id.is_nil()
            && valid_connected_folder_path(&self.path, false)
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

/// Validate a canonical model payload before it is durably checkpointed.
#[must_use]
pub fn validate_import_connected_file_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<ImportConnectedFileArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

/// Validate one canonical (id-bearing) output write-back checkpoint payload.
///
/// This checks the durable form the agent writes after resolving the model's
/// filename proposal, not the model-facing schema advertised by
/// [`write_output_to_connected_folder_tool_spec`].
#[must_use]
pub fn validate_write_output_to_connected_folder_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<WriteOutputToConnectedFolderArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

#[must_use]
pub fn list_connected_folders_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ListConnectedFoldersArgs>(
        LIST_CONNECTED_FOLDERS_TOOL,
        "List folders already connected to this conversation. Results contain opaque root IDs, display names, and the capabilities the trusted host currently grants on each folder; use request_folder_access to ask the user to choose another folder.",
    )
}

#[must_use]
pub fn list_folder_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ListFolderArgs>(
        LIST_FOLDER_TOOL,
        "List a directory below an already connected folder. Use only an opaque root_id and a root-relative path; never use an absolute path or parent traversal.",
    )
}

#[must_use]
pub fn read_connected_file_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ReadConnectedFileArgs>(
        READ_CONNECTED_FILE_TOOL,
        "Read a UTF-8 text file below an already connected folder. Use only an opaque root_id and a nonempty root-relative path; never use an absolute path or parent traversal.",
    )
}

#[must_use]
pub fn import_connected_file_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ImportConnectedFileArgs>(
        IMPORT_CONNECTED_FILE_TOOL,
        "Add one file below an already connected folder to this conversation as a source, so it can be read and cited. Use this for a PDF, Office document, or any other file that read_connected_file cannot return as text. Use only an opaque root_id and a nonempty root-relative path; never use an absolute path or parent traversal. Importing the same file again recovers the same single source rather than adding a duplicate. The completed result reports whether the source contains readable text.",
    )
}

#[must_use]
pub fn write_output_to_connected_folder_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<WriteOutputToConnectedFolderProposal>(
        WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
        "Copy an output already published in this conversation into a folder already connected to this conversation. Provide only the output's filename exactly as reported when it was published, the opaque root_id, a bounded root-relative destination, and explicit create or replace intent. Create refuses an existing entry, and may still need user approval depending on the conversation's permission mode. Replace always requires fresh user approval; never provide output bytes or an absolute path.",
    )
}

pub(crate) fn valid_connected_folder_path(path: &str, allow_root: bool) -> bool {
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
        assert_eq!(
            spec.input_schema["properties"]["reason"]["maxLength"],
            MAX_FOLDER_ACCESS_REASON_CHARS
        );
        assert_eq!(
            spec.input_schema["properties"]["requested_capabilities"]["items"],
            serde_json::json!({"enum": ["read_files"]})
        );
        assert_eq!(
            spec.input_schema["properties"]["folder_hint"],
            serde_json::json!({
                "enum": ["documents", "downloads"],
                "description": "Optional well-known picker hint, never an absolute path."
            })
        );
        assert!(spec.description.contains("grants no access"));
    }

    #[test]
    fn folder_access_results_never_need_a_host_path() {
        let result = RequestFolderAccessResult::Connected {
            root_id: uuid::Uuid::new_v4(),
            display_name: "Documents".into(),
            capabilities: vec![
                GrantedFolderCapability::ReadFiles,
                GrantedFolderCapability::WriteFiles,
                GrantedFolderCapability::ExecuteCommands,
            ],
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
    fn import_takes_the_same_bounded_pathless_proposal_as_a_read() {
        let root_id = uuid::Uuid::new_v4();
        assert!(validate_import_connected_file_arguments(
            &serde_json::json!({ "root_id": root_id, "path": "reports/q3.pdf" })
        ));
        // The folder root is a directory, not an importable file.
        assert!(!validate_import_connected_file_arguments(
            &serde_json::json!({ "root_id": root_id, "path": "" })
        ));
        for path in ["/tmp/secret.pdf", "../secret.pdf", "a/../secret", "a\\b"] {
            assert!(
                !validate_import_connected_file_arguments(
                    &serde_json::json!({ "root_id": root_id, "path": path })
                ),
                "{path}"
            );
        }
        // No way to name a media type, title, or conversation: the trusted
        // client derives all three.
        assert!(!validate_import_connected_file_arguments(
            &serde_json::json!({
                "root_id": root_id,
                "path": "a.pdf",
                "media_type": "application/pdf"
            })
        ));
        assert!(!validate_import_connected_file_arguments(
            &serde_json::json!({
                "root_id": uuid::Uuid::nil(),
                "path": "a.pdf"
            })
        ));
    }

    #[test]
    fn import_results_carry_a_source_id_but_never_a_host_path() {
        let result = ImportConnectedFileResult::Imported {
            document_id: uuid::Uuid::new_v4(),
            title: "q3.pdf".into(),
            media_type: "application/pdf".into(),
            bytes: 2_048,
            readiness: crate::SourceReadiness::StoredNoText,
        };
        let encoded = serde_json::to_value(&result).unwrap();
        assert_eq!(encoded["status"], "imported");
        assert_eq!(encoded["readiness"], "stored_no_text");
        assert!(encoded.get("path").is_none());
        assert!(encoded.get("root_id").is_none());
        assert_eq!(
            serde_json::from_value::<ImportConnectedFileResult>(encoded).unwrap(),
            result
        );
        assert_eq!(
            serde_json::to_value(ImportConnectedFileResult::Unavailable {
                message: "That file is no longer available to this conversation.".into()
            })
            .unwrap(),
            serde_json::json!({
                "status": "unavailable",
                "message": "That file is no longer available to this conversation."
            })
        );
    }

    #[test]
    fn output_writeback_names_only_durable_identities_and_a_relative_destination() {
        let output_id = uuid::Uuid::new_v4();
        let root_id = uuid::Uuid::new_v4();
        assert!(validate_write_output_to_connected_folder_arguments(
            &serde_json::json!({
                "output_id": output_id,
                "root_id": root_id,
                "path": "reports/final.md",
                "mode": "create"
            })
        ));
        for invalid in [
            serde_json::json!({
                "output_id": output_id,
                "root_id": root_id,
                "path": "../final.md",
                "mode": "create"
            }),
            serde_json::json!({
                "output_id": output_id,
                "root_id": root_id,
                "path": "/tmp/final.md",
                "mode": "replace"
            }),
            serde_json::json!({
                "output_id": output_id,
                "root_id": root_id,
                "path": "final.md",
                "mode": "create",
                "content": "forbidden"
            }),
        ] {
            assert!(!validate_write_output_to_connected_folder_arguments(
                &invalid
            ));
        }
        // The model-facing spec names outputs by filename: the exec surface
        // reports published outputs by filename only, so an id-shaped argument
        // would be impossible to supply.
        let spec = write_output_to_connected_folder_tool_spec();
        assert_eq!(spec.name, WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL);
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert_eq!(
            spec.input_schema["required"],
            serde_json::json!(["filename", "root_id", "path", "mode"])
        );
        assert!(spec.input_schema["properties"].get("output_id").is_none());
        assert!(spec.description.contains("filename"));
        assert!(!spec.description.contains("output_id"));
        assert!(spec.description.contains("never provide output bytes"));
        assert!(!spec.description.contains("scratch"));
    }

    /// The one client-executed write that leaves the sandbox is the one place
    /// the chat's permission mode reaches client execution. `Ask` advertises
    /// that workspace mutations ask, and a create into a real folder is one;
    /// replacement asks everywhere, because no mode consents to destroying
    /// bytes the agent did not write.
    #[test]
    fn a_connected_folder_write_asks_exactly_where_the_mode_promises() {
        use crate::PermissionMode;

        for mode in [
            None,
            Some(PermissionMode::Plan),
            Some(PermissionMode::Ask),
            Some(PermissionMode::Auto),
            Some(PermissionMode::Allow),
        ] {
            assert!(OutputWriteMode::Replace.requires_user_decision(mode));
        }
        assert!(OutputWriteMode::Create.requires_user_decision(None));
        assert!(OutputWriteMode::Create.requires_user_decision(Some(PermissionMode::Ask)));
        assert!(OutputWriteMode::Create.requires_user_decision(Some(PermissionMode::Plan)));
        assert!(!OutputWriteMode::Create.requires_user_decision(Some(PermissionMode::Auto)));
        assert!(!OutputWriteMode::Create.requires_user_decision(Some(PermissionMode::Allow)));
    }

    #[test]
    fn output_writeback_proposal_takes_a_portable_filename_never_an_id() {
        let root_id = uuid::Uuid::new_v4();
        let proposal: WriteOutputToConnectedFolderProposal =
            serde_json::from_value(serde_json::json!({
                "filename": "final report.md",
                "root_id": root_id,
                "path": "reports/final.md",
                "mode": "create"
            }))
            .unwrap();
        assert!(proposal.is_well_formed());

        let mut invalid = proposal.clone();
        invalid.filename = "../escape.md".into();
        assert!(!invalid.is_well_formed());
        invalid = proposal.clone();
        invalid.filename = String::new();
        assert!(!invalid.is_well_formed());
        invalid = proposal;
        invalid.path = "/tmp/final.md".into();
        assert!(!invalid.is_well_formed());

        // An id-bearing payload is not a valid proposal, and a filename-bearing
        // payload is not a valid canonical checkpoint.
        assert!(
            serde_json::from_value::<WriteOutputToConnectedFolderProposal>(serde_json::json!({
                "output_id": uuid::Uuid::new_v4(),
                "root_id": root_id,
                "path": "final.md",
                "mode": "create"
            }))
            .is_err()
        );
        assert!(!validate_write_output_to_connected_folder_arguments(
            &serde_json::json!({
                "filename": "final.md",
                "root_id": root_id,
                "path": "final.md",
                "mode": "create"
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
        assert_eq!(
            directory.input_schema["properties"]["path"],
            serde_json::json!({
                "type": "string",
                "maxLength": MAX_CONNECTED_FOLDER_PATH_BYTES,
                "description": "Root-relative directory path; empty means the folder root."
            })
        );
        assert_eq!(
            file.input_schema["properties"]["path"],
            serde_json::json!({
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_CONNECTED_FOLDER_PATH_BYTES,
                "description": "Root-relative text file path."
            })
        );
        assert!(!directory.description.contains("project_id"));
        assert!(!file.description.contains("grant"));
    }
}
