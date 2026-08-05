//! Closed contract for profile-scoped local apps.
//!
//! A local app is a profile-owned record with an opaque [`AppId`] and an
//! ordered history of immutable revisions, following the conversation-output
//! discipline with one deliberate difference: an app has no owning chat. The
//! conversation that authored a revision is nullable provenance on the
//! revision row, so an app outlives every conversation that touched it.
//!
//! Each revision pairs an untrusted HTML bundle (bytes on disk, recorded here
//! as length + digest) with a trusted, structurally validated manifest naming
//! the mounted MCP tools the app may call.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::deliverable::RevisionProducer;
use crate::id::{AgentRunId, AppId, AppRevisionId, ChatId, ConnectedAppId, HostRootId, TurnId};

/// Stable name of the foreground tool that creates and revises local apps.
///
/// Lives beside the record contract rather than in the feature-gated tool
/// module because the renderer projection allowlist needs the name without
/// pulling in the capability filesystem.
pub const CREATE_APP_TOOL: &str = "create_app";

/// Profile-data directory holding immutable app bundle bytes.
///
/// Deliberately a profile-level root rather than any conversation's private
/// scratch: the app record is profile-scoped and its bytes must survive the
/// originating conversation.
pub const APPS_DIRECTORY: &str = "apps";
/// Largest app bundle one revision may record — the same bound as a prefetched
/// MCP view document.
pub const MAX_APP_BUNDLE_BYTES: usize = 1024 * 1024;
/// Largest serialized manifest one revision may record.
pub const MAX_APP_MANIFEST_BYTES: usize = 64 * 1024;
/// Largest number of revisions one app retains.
///
/// Reaching the bound is a product decision rather than a storage failure: the
/// store refuses a further revision so no caller can silently lose history.
pub const MAX_APP_REVISIONS: u32 = 100;
/// Largest app display name accepted by the manifest.
pub const MAX_APP_NAME_CHARS: usize = 120;
/// Provider-safe bound every mounted tool name already obeys, reused verbatim
/// for manifest bindings so a pinned name can always match a mounted one.
pub const MAX_MOUNTED_TOOL_NAME_BYTES: usize = 64;
/// Largest `operationId` a `rest_api` binding may pin, in bytes.
///
/// A deliberate mirror of the OpenAPI ingest module's bound
/// (`openwave_server::openapi_catalog::MAX_OPERATION_ID_BYTES`): the catalog
/// enforces the same length and `[A-Za-z0-9_.-]` charset on every ingested
/// operation, so a pinned id within this grammar can always match an ingested
/// one. Duplicated here rather than imported because the manifest contract
/// must not depend on server code.
pub const MAX_OPERATION_ID_BYTES: usize = 128;

/// Profile-data location of one immutable revision's bundle bytes.
///
/// Bundle files are written once and never replaced, so the path is derived
/// entirely from durable identity — a display name can never steer where
/// bytes are written or read. Callers resolve it below the profile data
/// directory, never below any conversation's private scratch.
#[must_use]
pub fn app_revision_relative_path(app_id: AppId, revision_id: AppRevisionId) -> PathBuf {
    PathBuf::from(APPS_DIRECTORY)
        .join(app_id.to_string())
        .join(revision_id.to_string())
}

/// Publish bundle bytes at the write-once derived path under the profile data
/// directory.
///
/// Reuses the immutable-publication primitive the output record uses: atomic,
/// no-replace, symlink-refusing, and safe to retry with identical bytes.
#[cfg(feature = "tools")]
pub async fn publish_app_bundle(
    profile_dir: &cap_std::fs::Dir,
    app_id: AppId,
    revision_id: AppRevisionId,
    content: &[u8],
) -> crate::error::Result<()> {
    use crate::error::AgentError;

    let relative_path = app_revision_relative_path(app_id, revision_id);
    let profile_dir = profile_dir.try_clone().map_err(|error| {
        AgentError::Store(format!(
            "could not open the profile data directory: {error}"
        ))
    })?;
    let content = content.to_vec();
    tokio::task::spawn_blocking(move || {
        crate::tools::private_scratch::publish_immutable_file(
            &profile_dir,
            &relative_path,
            &content,
        )
    })
    .await
    .map_err(|error| AgentError::Store(format!("publication task failed: {error}")))?
    .map_err(|error| AgentError::Store(format!("could not publish app bundle: {error}")))
}

/// One profile-owned app and its current revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRecord {
    /// Durable opaque identity, stable across every revision.
    pub id: AppId,
    /// Display name, following the current revision's manifest. Not identity.
    pub name: String,
    /// Revision currently presented as the app's content.
    pub current_revision: AppRevisionId,
    /// Number of retained revisions, always at least one.
    pub revision_count: u32,
    /// Creation time of the first revision.
    pub created_at: DateTime<Utc>,
    /// Creation time of the current revision.
    pub updated_at: DateTime<Utc>,
    /// Set when the user deleted the app. Revisions are retained so a deletion
    /// stays recoverable.
    pub deleted_at: Option<DateTime<Utc>>,
}

/// One immutable revision of a local app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRevision {
    /// Durable identity, also the profile-data filename of its bundle bytes.
    pub id: AppRevisionId,
    /// App this revision belongs to.
    pub app_id: AppId,
    /// One-based position in the app's revision history.
    pub ordinal: u32,
    /// The trusted manifest: display name and pinned tool bindings.
    pub manifest: AppManifest,
    /// Exact byte length of the revision's bundle.
    pub byte_len: u64,
    /// SHA-256 of the bundle, used to recognize an exact retry.
    pub sha256: [u8; 32],
    /// Turn that produced the revision, when it came from a foreground turn.
    /// Mutually exclusive with [`AppRevision::producing_run_id`].
    pub turn_id: Option<TurnId>,
    /// Background run that produced the revision, when one did. Mutually
    /// exclusive with [`AppRevision::turn_id`].
    pub producing_run_id: Option<AgentRunId>,
    /// Conversation the revision was authored in, recorded as provenance only.
    ///
    /// Deliberately not a foreign key: the app is profile-scoped and must
    /// outlive the conversation, so this id may dangle after a chat deletion.
    pub chat_id: Option<ChatId>,
    /// Host-stamped creation time.
    pub created_at: DateTime<Utc>,
}

/// Content-identifying fields of a revision whose bundle the caller publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAppRevision {
    /// Caller-minted identity. Reusing it with the same content is an exact
    /// retry; reusing it with different content is rejected.
    pub id: AppRevisionId,
    /// The revision's manifest, validated structurally by the store.
    pub manifest: AppManifest,
    /// Exact byte length of the bundle.
    pub byte_len: u64,
    /// SHA-256 of the bundle.
    pub sha256: [u8; 32],
    /// Foreground turn that produced the revision, when one did. Mutually
    /// exclusive with `producing_run_id`.
    pub turn_id: Option<TurnId>,
    /// Background run that produced the revision, when one did. Mutually
    /// exclusive with `turn_id`.
    pub producing_run_id: Option<AgentRunId>,
    /// Originating conversation, recorded as nullable provenance.
    pub chat_id: Option<ChatId>,
    /// Host-stamped creation time.
    pub created_at: DateTime<Utc>,
}

impl NewAppRevision {
    /// Record the producer that minted this revision into its turn/run fields.
    #[must_use]
    pub fn with_producer(mut self, producer: RevisionProducer) -> Self {
        self.turn_id = producer.turn_id();
        self.producing_run_id = producer.producing_run_id();
        self
    }
}

/// Request to create an app together with its first revision.
///
/// The app's display name comes from the revision's manifest, so the record
/// and its manifest can never disagree about what the app is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateApp {
    /// Caller-minted identity, so an ambiguous store response can be retried.
    pub id: AppId,
    /// The app's first revision.
    pub revision: NewAppRevision,
}

/// The durable consent object for one app: the granted binding set —
/// `(app, tools[])` or `(app, operation_ids[])` per bound connected app —
/// with each bound connected app pinned to a fingerprint of its definition as
/// it was configured at consent time.
///
/// One grant per app, replaced wholesale by a fresh consent and deleted by
/// revocation. The fingerprint is what keeps consent honest: a Settings edit
/// can swap the definition behind a stable record, so enforcement compares
/// the granted fingerprint against the current definition on every invoke and
/// treats any difference as a stale grant, never as a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppGrant {
    /// App the consent belongs to.
    pub app_id: AppId,
    /// The granted bindings, computed by the host from the manifest and the
    /// connected-app definitions current at consent time — never supplied by
    /// a renderer.
    pub bindings: Vec<AppGrantBinding>,
    /// Host-stamped consent time.
    pub created_at: DateTime<Utc>,
}

/// One granted binding: the capabilities the user consented to under one
/// connected app, pinned to the definition that record carried at consent.
///
/// Untagged, mirroring [`AppBinding`]: the variants differ in exactly one
/// field name, and unknown fields refuse both shapes, so a persisted grant
/// binding deserializes to exactly the vocabulary it was granted under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AppGrantBinding {
    /// `{ app, tools[], fingerprint }` — granted mounted MCP tools.
    Tools(AppToolsGrantBinding),
    /// `{ app, operation_ids[], fingerprint }` — granted REST operations.
    Operations(AppOperationsGrantBinding),
    /// `{ folder, access, fingerprint }` — granted folder access.
    Folder(AppFolderGrantBinding),
}

impl AppGrantBinding {
    /// The connected app the grant binding names, when it names one — a
    /// folder grant binding names a broker root instead.
    #[must_use]
    pub fn app(&self) -> Option<ConnectedAppId> {
        match self {
            Self::Tools(binding) => Some(binding.app),
            Self::Operations(binding) => Some(binding.app),
            Self::Folder(_) => None,
        }
    }

    /// The definition fingerprint pinned at consent time.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        match self {
            Self::Tools(binding) => binding.fingerprint,
            Self::Operations(binding) => binding.fingerprint,
            Self::Folder(binding) => binding.fingerprint,
        }
    }
}

/// The granted mounted tools under one `mcp_server` connected app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppToolsGrantBinding {
    /// Connected app the consent names, matching the manifest binding it
    /// covers.
    pub app: ConnectedAppId,
    /// Full mounted tool names (`mcp__{namespace}__{tool}`) the grant covers.
    pub tools: Vec<String>,
    /// SHA-256 fingerprint of the connected app's definition as configured at
    /// consent time, computed by the host over a canonical serialization that
    /// carries configuration *names and structure* only — never environment
    /// or token values. Persisted as lowercase hex.
    #[serde(with = "hex_fingerprint")]
    pub fingerprint: [u8; 32],
}

/// The granted declared operations under one `rest_api` connected app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppOperationsGrantBinding {
    /// Connected app the consent names, matching the manifest binding it
    /// covers.
    pub app: ConnectedAppId,
    /// Catalog `operationId`s the grant covers.
    pub operation_ids: Vec<String>,
    /// SHA-256 fingerprint of the connected app's definition as configured at
    /// consent time — for a `rest_api` record, over the base URL, document
    /// hash, and credential *reference* and placement, never a credential
    /// value. Persisted as lowercase hex.
    #[serde(with = "hex_fingerprint")]
    pub fingerprint: [u8; 32],
}

/// The granted access to one connected folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppFolderGrantBinding {
    /// Broker root the consent names, matching the manifest binding it
    /// covers.
    pub folder: HostRootId,
    /// The granted access level.
    pub access: FolderAccess,
    /// SHA-256 fingerprint over the folder grant's canonical form — the root
    /// id and access level, never a path or display name. Persisted as
    /// lowercase hex.
    #[serde(with = "hex_fingerprint")]
    pub fingerprint: [u8; 32],
}

/// Lowercase-hex persistence for a grant binding's definition fingerprint.
mod hex_fingerprint {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        use std::fmt::Write as _;

        let mut text = String::with_capacity(64);
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        serializer.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(deserializer)?;
        let malformed = || serde::de::Error::custom("malformed definition fingerprint");
        if text.len() != 64 || !text.is_ascii() {
            return Err(malformed());
        }
        let mut bytes = [0_u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte =
                u8::from_str_radix(&text[2 * index..2 * index + 2], 16).map_err(|_| malformed())?;
        }
        Ok(bytes)
    }
}

/// Validate a grant's bindings structurally and return the JSON to store.
///
/// A grant is host-computed from an already-validated manifest, so this is the
/// same binding grammar [`validate_app_manifest`] enforces — repeated at the
/// storage door so no other write path can persist a binding the manifest
/// validator would have refused.
pub fn validate_app_grant(grant: &AppGrant) -> Result<serde_json::Value, String> {
    validate_binding_set(grant.bindings.iter().map(|binding| match binding {
        AppGrantBinding::Tools(binding) => (
            BindingKey::App(binding.app),
            BindingCapabilities::Tools(&binding.tools),
        ),
        AppGrantBinding::Operations(binding) => (
            BindingKey::App(binding.app),
            BindingCapabilities::Operations(&binding.operation_ids),
        ),
        AppGrantBinding::Folder(binding) => (
            BindingKey::Folder(binding.folder),
            BindingCapabilities::Folder,
        ),
    }))?;
    serde_json::to_value(&grant.bindings).map_err(|error| format!("unencodable app grant: {error}"))
}

/// The trusted half of an app revision: a display name and the exact mounted
/// tools the app may call.
///
/// The manifest — not the bundle — is what the user consents to and what the
/// host enforces per call, so its shape is closed (`deny_unknown_fields`) and
/// validated structurally before anything is stored.
/// `JsonSchema` because the `create_app` tool takes the manifest as a typed
/// argument, so the model sees the exact structural contract the store will
/// validate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppManifest {
    /// Display name shown in the transcript card and the Apps library.
    pub name: String,
    /// Pinned tool bindings, grouped by connected app.
    pub bindings: Vec<AppBinding>,
}

/// The capabilities one binding contributes to a local app: declared REST
/// operations of a `rest_api` connected app, bounded access to a connected
/// folder, or — retired (#1332) but still parseable — mounted MCP tools.
///
/// Untagged and closed: the shapes are distinguished by their differing
/// field names, each variant refuses unknown fields, and a body mixing
/// fields from two shapes matches none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AppBinding {
    /// `{ app, tools[] }` — mounted MCP tools of an `mcp_server` record.
    Tools(AppToolsBinding),
    /// `{ app, operation_ids[] }` — declared operations of a `rest_api`
    /// record.
    Operations(AppOperationsBinding),
    /// `{ folder, access }` — bounded access to a connected folder.
    Folder(AppFolderBinding),
}

impl AppBinding {
    /// The connected app the binding names, when it names one — a folder
    /// binding names a broker root instead.
    #[must_use]
    pub fn app(&self) -> Option<ConnectedAppId> {
        match self {
            Self::Tools(binding) => Some(binding.app),
            Self::Operations(binding) => Some(binding.app),
            Self::Folder(_) => None,
        }
    }
}

/// The mounted tools one `mcp_server` connected app contributes to a local
/// app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppToolsBinding {
    /// Id of the connected app the tools belong to. The available ids are
    /// listed in the `create_app` tool description.
    #[schemars(description = "Id of the mcp_server connected app these tools belong to.")]
    pub app: ConnectedAppId,
    /// Full mounted tool names, each `mcp__{namespace}__{tool}` under the
    /// bound connected app's namespace.
    #[schemars(
        description = "Full mounted tool names (`mcp__{namespace}__{tool}`), all \
                       under the bound connected app's namespace."
    )]
    pub tools: Vec<String>,
}

/// The declared operations one `rest_api` connected app contributes to a
/// local app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppOperationsBinding {
    /// Id of the connected app the operations belong to. The available ids
    /// are listed in the `create_app` tool description.
    #[schemars(description = "Id of the rest_api connected app these operations belong to.")]
    pub app: ConnectedAppId,
    /// Catalog `operationId`s of the bound connected app's ingested OpenAPI
    /// document.
    #[schemars(description = "OpenAPI operationIds declared by the bound rest_api \
                       connected app's catalog.")]
    pub operation_ids: Vec<String>,
}

/// Bounded access to one connected folder, by broker root id.
///
/// The folder is not a connected app: its identity is the host broker's
/// registration, established through the trusted native picker, and the
/// binding names it by the same opaque root id every other product surface
/// uses. See `docs/folder-bindings.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppFolderBinding {
    /// Root id of the connected folder. The available ids are listed in the
    /// `create_app` tool description.
    #[schemars(description = "Root id of the approved connected folder.")]
    pub folder: HostRootId,
    /// The access level the app requests over the folder.
    #[schemars(description = "Access the app requests: \"read\" for listing and \
                       bounded reads, \"read_write\" to also write files. Write \
                       access is a louder consent — request it only when the app \
                       needs it.")]
    pub access: FolderAccess,
}

/// The access level of a folder binding.
///
/// Consent-bearing: the level is part of what the user grants and part of
/// the binding's fingerprint, so widening `read` to `read_write` always
/// re-prompts.
/// The access level of a folder binding.
///
/// Consent-bearing: the level is part of what the user grants and part of
/// the binding's fingerprint, so widening `read` to `read_write` always
/// re-prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum FolderAccess {
    /// Listing and bounded reads.
    Read,
    /// Listing, bounded reads, and bounded writes.
    ReadWrite,
}

/// Hand-written rather than derived: the derive renders the documented
/// variants as a `oneOf` of `const` entries, a form whose wrapper node has
/// no `type`, `enum`, or `anyOf` — and provider schema translators (Gemini's
/// bounded subset) refuse exactly that shape. A plain string enum says the
/// same thing in the subset every provider speaks.
impl schemars::JsonSchema for FolderAccess {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "FolderAccess".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "enum": ["read", "read_write"],
        })
    }
}

impl FolderAccess {
    /// Stable wire spelling, for canonical forms and display.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "read_write",
        }
    }
}

/// Best-effort authoring-time lookup of the host's approved connected
/// folders, for the `create_app` door and its roster.
///
/// Legibility, not the gate: the consent and invoke surfaces resolve live
/// against the host. An embedding without a host (or one whose lookup
/// fails) answers empty, and the door refuses folder bindings with that
/// honestly.
#[async_trait::async_trait]
pub trait ApprovedFolderSource: Send + Sync {
    /// Every approved connected folder as `(root id, display name)`; empty
    /// on any error.
    async fn approved_folders(&self) -> Vec<(HostRootId, String)>;
}

/// Validate an app manifest structurally and return the JSON to store.
///
/// Checks the display name, the mounted-tool grammar of every binding, and the
/// serialized size bound. Tool names must be full mounted names under the
/// binding's server namespace and fit the provider-safe 64-byte
/// `[A-Za-z0-9_-]` contract every mounted tool already obeys, so a pinned name
/// that could never match a mounted tool is refused at the door.
pub fn validate_app_manifest(manifest: &AppManifest) -> Result<serde_json::Value, String> {
    validate_app_name(&manifest.name)?;
    validate_binding_set(manifest.bindings.iter().map(|binding| match binding {
        AppBinding::Tools(binding) => (
            BindingKey::App(binding.app),
            BindingCapabilities::Tools(&binding.tools),
        ),
        AppBinding::Operations(binding) => (
            BindingKey::App(binding.app),
            BindingCapabilities::Operations(&binding.operation_ids),
        ),
        AppBinding::Folder(binding) => (
            BindingKey::Folder(binding.folder),
            BindingCapabilities::Folder,
        ),
    }))?;
    let json =
        serde_json::to_value(manifest).map_err(|error| format!("unencodable manifest: {error}"))?;
    let encoded_len = json.to_string().len();
    if encoded_len > MAX_APP_MANIFEST_BYTES {
        return Err(format!(
            "manifest is too large ({encoded_len} bytes, maximum {MAX_APP_MANIFEST_BYTES})"
        ));
    }
    Ok(json)
}

/// One binding's identity for the duplicate check: the connected app it
/// binds, or the folder root it binds.
#[derive(PartialEq, Eq, Hash)]
enum BindingKey {
    App(ConnectedAppId),
    Folder(HostRootId),
}

/// One binding's capability list, by vocabulary, for the shared grammar check.
enum BindingCapabilities<'a> {
    Tools(&'a [String]),
    Operations(&'a [String]),
    Folder,
}

/// The binding grammar shared by manifests and grants: no duplicate connected
/// apps or folders (one binding per record or root, whichever vocabulary),
/// every tool shaped like a full mounted name, and every operation id within
/// the ingest module's `operationId` grammar. A folder binding has no list to
/// validate — its access level is closed by type.
///
/// A namespace may itself contain `_`, so a mounted name cannot be split
/// unambiguously without knowing the namespace — and the bound record lives
/// behind the store. The grammar here is therefore structural only; the exact
/// `mcp__{namespace}__` cross-check against the bound connected app's
/// configuration — and the catalog membership check for operation ids —
/// belongs to the host layers that resolve ids (the `create_app` door, grant
/// computation, the invoke gate).
fn validate_binding_set<'a>(
    bindings: impl Iterator<Item = (BindingKey, BindingCapabilities<'a>)>,
) -> Result<(), String> {
    let mut keys = std::collections::HashSet::new();
    for (key, capabilities) in bindings {
        match &key {
            BindingKey::App(app) => {
                if keys.contains(&key) {
                    return Err(format!("duplicate binding for connected app {app}"));
                }
            }
            BindingKey::Folder(folder) => {
                if keys.contains(&key) {
                    return Err(format!("duplicate binding for folder {folder}"));
                }
            }
        }
        keys.insert(key);
        match capabilities {
            BindingCapabilities::Tools(binding_tools) => {
                let mut tools = std::collections::HashSet::new();
                for tool in binding_tools {
                    if tool.len() > MAX_MOUNTED_TOOL_NAME_BYTES || !is_mounted_name_charset(tool) {
                        return Err(format!(
                            "tool {tool:?} must be at most {MAX_MOUNTED_TOOL_NAME_BYTES} bytes of [A-Za-z0-9_-]"
                        ));
                    }
                    if !is_mounted_name_shape(tool) {
                        return Err(format!(
                            "tool {tool:?} is not a mounted `mcp__{{namespace}}__{{tool}}` name"
                        ));
                    }
                    if !tools.insert(tool.as_str()) {
                        return Err(format!("duplicate tool {tool:?}"));
                    }
                }
            }
            BindingCapabilities::Operations(binding_operations) => {
                let mut operations = std::collections::HashSet::new();
                for operation_id in binding_operations {
                    if operation_id.is_empty()
                        || operation_id.len() > MAX_OPERATION_ID_BYTES
                        || !is_operation_id_charset(operation_id)
                    {
                        return Err(format!(
                            "operation id {operation_id:?} must be 1 to \
                             {MAX_OPERATION_ID_BYTES} bytes of [A-Za-z0-9_.-]"
                        ));
                    }
                    if !operations.insert(operation_id.as_str()) {
                        return Err(format!("duplicate operation id {operation_id:?}"));
                    }
                }
            }
            // A folder binding carries no capability list; the access level
            // is closed by type and consent-bearing rather than grammatical.
            BindingCapabilities::Folder => {}
        }
    }
    Ok(())
}

/// Whether a name is shaped like `mcp__{namespace}__{tool}` with a non-empty
/// namespace and tool segment under *some* split — the namespace itself may
/// contain `_`, so the exact split is only decidable against a configured
/// record.
fn is_mounted_name_shape(name: &str) -> bool {
    let Some(qualified) = name.strip_prefix("mcp__") else {
        return false;
    };
    qualified
        .match_indices("__")
        .any(|(index, _)| index > 0 && index + 2 < qualified.len())
}

/// The bare tool segment of `name` when it is mounted under exactly
/// `namespace` — the host-side half of the binding grammar, shared by the
/// `create_app` door, grant computation, and the invoke gate so every layer
/// applies the same exact-prefix reading.
#[must_use]
pub fn mounted_tool_under<'a>(namespace: &str, name: &'a str) -> Option<&'a str> {
    name.strip_prefix("mcp__")?
        .strip_prefix(namespace)?
        .strip_prefix("__")
        .filter(|tool| !tool.is_empty())
}

fn validate_app_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > MAX_APP_NAME_CHARS {
        return Err(format!(
            "app name must contain between 1 and {MAX_APP_NAME_CHARS} characters"
        ));
    }
    if name.trim() != name {
        return Err("app name may not have surrounding whitespace".into());
    }
    if name.chars().any(char::is_control) {
        return Err("app name may not contain control characters".into());
    }
    Ok(())
}

fn is_mounted_name_charset(name: &str) -> bool {
    name.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// The ingest module's `operationId` charset, mirrored (see
/// [`MAX_OPERATION_ID_BYTES`]).
fn is_operation_id_charset(operation_id: &str) -> bool {
    operation_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_paths_are_derived_only_from_durable_identity() {
        let app_id = AppId::new();
        let revision_id = AppRevisionId::new();
        let path = app_revision_relative_path(app_id, revision_id);

        assert_eq!(
            path,
            PathBuf::from(APPS_DIRECTORY)
                .join(app_id.to_string())
                .join(revision_id.to_string())
        );
        assert!(path.is_relative());
        // The derivation takes no display name at all, so renaming an app can
        // never steer where bundle bytes are written or read.
        assert_eq!(
            path,
            app_revision_relative_path(app_id, revision_id),
            "the same identity always resolves to the same path"
        );
        assert_ne!(
            path,
            app_revision_relative_path(app_id, AppRevisionId::new()),
            "each revision owns a distinct, write-once path"
        );
    }

    #[test]
    fn manifests_enforce_the_mounted_tool_grammar() {
        let app = ConnectedAppId::new();
        let manifest = |tools: &[&str]| AppManifest {
            name: "Sentry triage".into(),
            bindings: vec![AppBinding::Tools(AppToolsBinding {
                app,
                tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
            })],
        };

        assert!(validate_app_manifest(&manifest(&[
            "mcp__sentry__list_issues",
            "mcp__sentry__get_issue"
        ]))
        .is_ok());
        // Bindings may be empty: a static app pins no tools.
        assert!(validate_app_manifest(&AppManifest {
            name: "Static".into(),
            bindings: Vec::new(),
        })
        .is_ok());

        for tool in [
            "list_issues",                               // bare name, not mounted
            "mcp__sentry__",                             // empty tool segment
            "mcp____tool",                               // empty namespace segment
            "mcp__sentry__bad name",                     // charset
            &format!("mcp__sentry__{}", "x".repeat(64)), // over the 64-byte bound
        ] {
            assert!(validate_app_manifest(&manifest(&[tool])).is_err(), "{tool}");
        }
        assert!(
            validate_app_manifest(&manifest(&[
                "mcp__sentry__list_issues",
                "mcp__sentry__list_issues"
            ]))
            .is_err(),
            "duplicate pins are refused"
        );
        assert!(
            validate_app_manifest(&AppManifest {
                name: "Twice".into(),
                bindings: vec![
                    AppBinding::Tools(AppToolsBinding {
                        app,
                        tools: Vec::new()
                    }),
                    AppBinding::Tools(AppToolsBinding {
                        app,
                        tools: Vec::new()
                    }),
                ],
            })
            .is_err(),
            "duplicate connected-app bindings are refused"
        );

        for name in ["", " padded ", "line\nbreak", &"x".repeat(121)] {
            assert!(
                validate_app_manifest(&AppManifest {
                    name: (*name).to_owned(),
                    bindings: Vec::new(),
                })
                .is_err(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn manifests_enforce_the_operation_id_grammar() {
        let app = ConnectedAppId::new();
        let manifest = |operation_ids: &[&str]| AppManifest {
            name: "Issue browser".into(),
            bindings: vec![AppBinding::Operations(AppOperationsBinding {
                app,
                operation_ids: operation_ids.iter().map(|id| (*id).to_owned()).collect(),
            })],
        };

        assert!(validate_app_manifest(&manifest(&["listIssues", "get.issue-v2_1"])).is_ok());
        for operation_id in [
            "",            // empty
            "bad id",      // charset (space)
            "issues/list", // charset (slash)
            &"x".repeat(MAX_OPERATION_ID_BYTES + 1),
        ] {
            assert!(
                validate_app_manifest(&manifest(&[operation_id])).is_err(),
                "{operation_id:?}"
            );
        }
        assert!(
            validate_app_manifest(&manifest(&["listIssues", "listIssues"])).is_err(),
            "duplicate pins are refused"
        );
        // One binding per connected app holds across vocabularies: the same
        // record cannot be bound once for tools and once for operations.
        assert!(
            validate_app_manifest(&AppManifest {
                name: "Twice".into(),
                bindings: vec![
                    AppBinding::Tools(AppToolsBinding {
                        app,
                        tools: Vec::new()
                    }),
                    AppBinding::Operations(AppOperationsBinding {
                        app,
                        operation_ids: Vec::new()
                    }),
                ],
            })
            .is_err(),
            "duplicate connected-app bindings are refused across binding kinds"
        );
    }

    #[test]
    fn bindings_deserialize_untagged_and_closed() {
        use serde_json::json;

        let app = ConnectedAppId::new();
        let tools: AppBinding =
            serde_json::from_value(json!({ "app": app, "tools": ["mcp__srv__viewer"] })).unwrap();
        assert!(matches!(tools, AppBinding::Tools(_)));
        let operations: AppBinding =
            serde_json::from_value(json!({ "app": app, "operation_ids": ["listIssues"] })).unwrap();
        assert!(matches!(operations, AppBinding::Operations(_)));
        // Both vocabularies in one binding, or any unknown field, match
        // neither closed variant.
        for body in [
            json!({ "app": app, "tools": [], "operation_ids": [] }),
            json!({ "app": app, "tools": [], "extra": true }),
            json!({ "app": app }),
        ] {
            assert!(
                serde_json::from_value::<AppBinding>(body.clone()).is_err(),
                "{body}"
            );
        }

        // The grant vocabulary round-trips the same way, fingerprint included.
        let fingerprint = "ab".repeat(32);
        let granted: AppGrantBinding = serde_json::from_value(
            json!({ "app": app, "operation_ids": ["listIssues"], "fingerprint": fingerprint }),
        )
        .unwrap();
        assert!(matches!(granted, AppGrantBinding::Operations(_)));
        assert_eq!(granted.fingerprint(), [0xab; 32]);
        assert!(serde_json::from_value::<AppGrantBinding>(
            json!({ "app": app, "tools": [], "operation_ids": [], "fingerprint": fingerprint })
        )
        .is_err());
    }

    /// The folder vocabulary joins the same untagged, closed grammar: its
    /// shape parses, its access levels are a closed set, mixing it with an
    /// app-keyed shape matches nothing, and one binding per root holds like
    /// one binding per connected app.
    #[test]
    fn folder_bindings_parse_closed_and_dedupe_by_root() {
        use serde_json::json;

        let app = ConnectedAppId::new();
        let folder = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
        let read: AppBinding =
            serde_json::from_value(json!({ "folder": folder, "access": "read" })).unwrap();
        let AppBinding::Folder(binding) = &read else {
            panic!("a folder shape must parse as a folder binding");
        };
        assert_eq!(binding.access, FolderAccess::Read);
        assert!(read.app().is_none());
        let write: AppBinding =
            serde_json::from_value(json!({ "folder": folder, "access": "read_write" })).unwrap();
        assert!(matches!(
            write,
            AppBinding::Folder(AppFolderBinding {
                access: FolderAccess::ReadWrite,
                ..
            })
        ));

        // Closed on every edge: an open-ended access value, a mixed shape,
        // an unknown field, and a nil root id all refuse.
        for body in [
            json!({ "folder": folder, "access": "write" }),
            json!({ "folder": folder, "access": "read", "app": app }),
            json!({ "folder": folder, "access": "read", "extra": true }),
            json!({ "folder": folder }),
            json!({ "folder": uuid::Uuid::nil(), "access": "read" }),
        ] {
            assert!(
                serde_json::from_value::<AppBinding>(body.clone()).is_err(),
                "{body}"
            );
        }

        // One binding per folder root; a folder and a connected app never
        // collide on the duplicate check.
        let folder_binding =
            |access: FolderAccess| AppBinding::Folder(AppFolderBinding { folder, access });
        assert!(validate_app_manifest(&AppManifest {
            name: "Files".into(),
            bindings: vec![
                folder_binding(FolderAccess::Read),
                AppBinding::Operations(AppOperationsBinding {
                    app,
                    operation_ids: vec!["listIssues".into()],
                }),
            ],
        })
        .is_ok());
        assert!(
            validate_app_manifest(&AppManifest {
                name: "Files".into(),
                bindings: vec![
                    folder_binding(FolderAccess::Read),
                    folder_binding(FolderAccess::ReadWrite),
                ],
            })
            .is_err(),
            "duplicate folder bindings are refused across access levels"
        );

        // The grant twin round-trips with its pinned fingerprint and keeps
        // the untagged discrimination.
        let fingerprint = "cd".repeat(32);
        let granted: AppGrantBinding = serde_json::from_value(
            json!({ "folder": folder, "access": "read_write", "fingerprint": fingerprint }),
        )
        .unwrap();
        assert!(matches!(granted, AppGrantBinding::Folder(_)));
        assert!(granted.app().is_none());
        assert_eq!(granted.fingerprint(), [0xcd; 32]);
        assert!(serde_json::from_value::<AppGrantBinding>(
            json!({ "folder": folder, "access": "read" })
        )
        .is_err());
    }

    #[test]
    fn the_exact_namespace_reading_is_prefix_anchored() {
        // A namespace may contain `_`, so only the exact-prefix reading is
        // sound — this is the check the host layers apply once the bound
        // record's namespace is known.
        assert_eq!(
            mounted_tool_under("my_server", "mcp__my_server__tool"),
            Some("tool")
        );
        assert_eq!(mounted_tool_under("my", "mcp__my_server__tool"), None);
        assert_eq!(mounted_tool_under("sentry", "mcp__github__list"), None);
        assert_eq!(mounted_tool_under("sentry", "mcp__sentry__"), None);
        assert_eq!(mounted_tool_under("sentry", "sentry__tool"), None);
    }
}
