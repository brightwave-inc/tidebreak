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
//! the connected-app operations, folders, and gateway-app operations the app
//! may call.

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
/// Largest `operationId` a `rest_api` binding may pin, in bytes.
///
/// A deliberate mirror of the OpenAPI ingest module's bound
/// (`tidebreak_server::openapi_catalog::MAX_OPERATION_ID_BYTES`): the catalog
/// enforces the same length and `[A-Za-z0-9_.-]` charset on every ingested
/// operation, so a pinned id within this grammar can always match an ingested
/// one. Duplicated here rather than imported because the manifest contract
/// must not depend on server code.
pub const MAX_OPERATION_ID_BYTES: usize = 128;
/// Largest gateway connected-app id a gateway binding may name, in bytes.
///
/// The id is the gateway's own identifier for the app — opaque here, and
/// deliberately not parsed as any particular identity shape: today's gateway
/// mints UUIDs, and a manifest that pinned that assumption would break the
/// day it mints anything else. The bound and charset only keep an id
/// printable, loggable, and small enough to store beside the rest of the
/// manifest.
pub const MAX_GATEWAY_APP_ID_BYTES: usize = 128;

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
    /// The trusted manifest: display name and pinned capability bindings.
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
/// `(app, operation_ids[])` per bound connected app, `(folder, access)` per
/// bound folder, or `(gateway_app, operation_ids[])` per bound gateway app —
/// with each bound target pinned to a fingerprint of its definition as it
/// stood at consent time.
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
/// Untagged, mirroring [`AppBinding`]: the variants are told apart by the
/// field naming their target (`app`, `folder`, `gateway_app`), and unknown
/// fields refuse every shape, so a persisted grant binding deserializes to
/// exactly the vocabulary it was granted under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AppGrantBinding {
    /// `{ app, operation_ids[], fingerprint }` — granted REST operations.
    Operations(AppOperationsGrantBinding),
    /// `{ folder, access, fingerprint }` — granted folder access.
    Folder(AppFolderGrantBinding),
    /// `{ gateway_app, operation_ids[], fingerprint }` — granted operations
    /// of a connected app the model gateway holds.
    GatewayOperations(AppGatewayOperationsGrantBinding),
}

impl AppGrantBinding {
    /// The locally configured connected app the grant binding names, when it
    /// names one — a folder grant binding names a broker root and a gateway
    /// grant binding names an app the gateway holds, neither of which is a
    /// local record.
    #[must_use]
    pub fn app(&self) -> Option<ConnectedAppId> {
        match self {
            Self::Operations(binding) => Some(binding.app),
            Self::Folder(_) | Self::GatewayOperations(_) => None,
        }
    }

    /// The gateway connected app the grant binding names, when it names one.
    #[must_use]
    pub fn gateway_app(&self) -> Option<&str> {
        match self {
            Self::GatewayOperations(binding) => Some(&binding.gateway_app),
            Self::Operations(_) | Self::Folder(_) => None,
        }
    }

    /// The definition fingerprint pinned at consent time.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        match self {
            Self::Operations(binding) => binding.fingerprint,
            Self::Folder(binding) => binding.fingerprint,
            Self::GatewayOperations(binding) => binding.fingerprint,
        }
    }
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

/// The granted operations of one connected app the model gateway holds.
///
/// The twin of [`AppGatewayOperationsBinding`], pinned like every other grant
/// binding: consent named a gateway app *as the gateway described it*, so a
/// re-ingested catalog or a re-pairing to a different gateway moves the
/// fingerprint and re-prompts. Entitlement is deliberately outside the pin —
/// it is the gateway's live predicate, re-evaluated per call, and losing it
/// fails the call rather than revoking the consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppGatewayOperationsGrantBinding {
    /// Gateway connected-app id the consent names, matching the manifest
    /// binding it covers.
    pub gateway_app: String,
    /// Operation ids the grant covers, as the gateway's catalog declares
    /// them.
    pub operation_ids: Vec<String>,
    /// SHA-256 fingerprint over the gateway app's canonical form — the
    /// gateway's origin, the app id, and the hash of the operation catalog it
    /// declared at consent time, never a credential (none exists locally for
    /// a gateway app). Persisted as lowercase hex.
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
        AppGrantBinding::Operations(binding) => (
            BindingKey::App(binding.app),
            BindingCapabilities::Operations(&binding.operation_ids),
        ),
        AppGrantBinding::Folder(binding) => (
            BindingKey::Folder(binding.folder),
            BindingCapabilities::Folder,
        ),
        AppGrantBinding::GatewayOperations(binding) => (
            BindingKey::GatewayApp(&binding.gateway_app),
            BindingCapabilities::Operations(&binding.operation_ids),
        ),
    }))?;
    serde_json::to_value(&grant.bindings).map_err(|error| format!("unencodable app grant: {error}"))
}

/// The gateway-side registration one local app holds at one deployment.
///
/// Registration is per deployment, not per profile: the gateway a profile is
/// paired to owns the shared app, so the base URL is half the identity. A
/// profile re-paired elsewhere holds no registration there and registers
/// afresh — exactly as it holds no gateway grant there.
///
/// `gateway_revision_id` is the gateway's own id for the revision the shared
/// app currently serves, and `synced_revision_id` is the local revision that
/// revision was projected from. The pair is what makes the sync lazy and
/// idempotent: a local revision the gateway has never seen is recognized by
/// `synced_revision_id` no longer matching the app's current revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppGatewayDraft {
    /// Local app the registration belongs to.
    pub app_id: AppId,
    /// Gateway deployment the registration lives at, normalized by the
    /// caller to the same form the session's base URL is compared in.
    pub gateway_base_url: String,
    /// The gateway's id for the shared app this local app was registered as.
    /// Opaque here, and held to the same grammar a manifest binding's gateway
    /// app id is: bounded, printable, and never interpreted.
    pub shared_app_id: String,
    /// The gateway's id for the revision the shared app currently serves.
    pub gateway_revision_id: String,
    /// The local revision `gateway_revision_id` was projected from.
    pub synced_revision_id: AppRevisionId,
    /// Host-stamped time of the last create or revision append.
    pub updated_at: DateTime<Utc>,
}

/// Largest gateway deployment URL a registration row may record.
pub const MAX_GATEWAY_BASE_URL_BYTES: usize = 2048;

/// Validate a gateway registration structurally, at the storage door.
///
/// The three opaque halves are the gateway's own identifiers, so they are
/// bounded and kept printable rather than parsed — the same posture a
/// manifest's gateway app id is held to, and the same reason: Tidebreak never
/// interprets them, but it does put them in URLs and logs.
pub fn validate_app_gateway_draft(draft: &AppGatewayDraft) -> Result<(), String> {
    if draft.gateway_base_url.is_empty()
        || draft.gateway_base_url.len() > MAX_GATEWAY_BASE_URL_BYTES
    {
        return Err(format!(
            "gateway base URL must be 1 to {MAX_GATEWAY_BASE_URL_BYTES} bytes"
        ));
    }
    validate_gateway_identifier("shared app id", &draft.shared_app_id)?;
    validate_gateway_identifier("gateway revision id", &draft.gateway_revision_id)
}

/// The bound and charset every opaque gateway identifier is held to.
fn validate_gateway_identifier(what: &str, value: &str) -> Result<(), String> {
    // Pure-dot values pass the printable-ASCII check but read as path
    // navigation to any origin that normalizes dot segments.
    if value.is_empty()
        || value.len() > MAX_GATEWAY_APP_ID_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
        || value.bytes().all(|byte| byte == b'.')
    {
        return Err(format!(
            "{what} {value:?} must be 1 to {MAX_GATEWAY_APP_ID_BYTES} bytes of printable, \
             non-whitespace ASCII"
        ));
    }
    Ok(())
}

/// The trusted half of an app revision: a display name and the exact
/// capabilities the app may call.
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
    /// Pinned capability bindings, grouped by connected app or folder.
    pub bindings: Vec<AppBinding>,
}

/// The capabilities one binding contributes to a local app: declared REST
/// operations of a `rest_api` connected app, bounded access to a connected
/// folder, or declared operations of a connected app the model gateway holds.
///
/// Untagged and closed: the shapes are distinguished by their differing
/// field names, each variant refuses unknown fields, and a body mixing
/// fields from two shapes matches none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AppBinding {
    /// `{ app, operation_ids[] }` — declared operations of a `rest_api`
    /// record.
    Operations(AppOperationsBinding),
    /// `{ folder, access }` — bounded access to a connected folder.
    Folder(AppFolderBinding),
    /// `{ gateway_app, operation_ids[] }` — declared operations of a
    /// connected app the model gateway holds.
    GatewayOperations(AppGatewayOperationsBinding),
}

impl AppBinding {
    /// The locally configured connected app the binding names, when it names
    /// one — a folder binding names a broker root and a gateway binding names
    /// an app the gateway holds, neither of which is a local record.
    #[must_use]
    pub fn app(&self) -> Option<ConnectedAppId> {
        match self {
            Self::Operations(binding) => Some(binding.app),
            Self::Folder(_) | Self::GatewayOperations(_) => None,
        }
    }

    /// The gateway connected app the binding names, when it names one.
    #[must_use]
    pub fn gateway_app(&self) -> Option<&str> {
        match self {
            Self::GatewayOperations(binding) => Some(&binding.gateway_app),
            Self::Operations(_) | Self::Folder(_) => None,
        }
    }
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

/// The declared operations one connected app of the model gateway
/// contributes to a local app.
///
/// The gateway app is named by the gateway's own connected-app id — the same
/// identifier the gateway's shared-app manifests bind — and nothing about it
/// resolves locally: no definition, no catalog, and no credential exists on
/// this machine. A binding here is a network binding, executed by relaying to
/// the gateway as the signed-in user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppGatewayOperationsBinding {
    /// Id of the gateway connected app the operations belong to, opaque to
    /// Tidebreak.
    #[schemars(description = "Id of the gateway connected app these operations belong to.")]
    pub gateway_app: String,
    /// Operation ids of the bound gateway app's declared catalog.
    #[schemars(description = "Operation ids declared by the bound gateway connected \
                       app's catalog.")]
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

/// One connected app of the model gateway, as the authoring door sees it.
pub struct GatewayAuthoringApp {
    /// The gateway's connected-app id — what a manifest binding names.
    pub id: String,
    /// Display name, for the refusal that lists what is bindable.
    pub name: String,
    /// The operation ids the gateway's catalog declares for the app.
    pub operation_ids: Vec<String>,
}

/// Best-effort authoring-time lookup of the gateway connected apps this
/// profile could bind, for the `create_app` door and its roster.
///
/// Legibility, not the gate: the consent and invoke surfaces resolve live
/// against the gateway. The core crate cannot see the gateway session, so the
/// server injects this.
#[async_trait::async_trait]
pub trait GatewayAppSource: Send + Sync {
    /// Every bindable gateway app, or `None` when this profile has no gateway
    /// session to answer with — which is a different statement from
    /// `Some(vec![])`, a session that answered with nothing entitled. The door
    /// refuses either way, but only the first can be fixed by signing in.
    async fn entitled_apps(&self) -> Option<Vec<GatewayAuthoringApp>>;
}

/// Validate an app manifest structurally and return the JSON to store.
///
/// Checks the display name, the grammar of every binding, and the serialized
/// size bound.
pub fn validate_app_manifest(manifest: &AppManifest) -> Result<serde_json::Value, String> {
    validate_app_name(&manifest.name)?;
    validate_binding_set(manifest.bindings.iter().map(|binding| match binding {
        AppBinding::Operations(binding) => (
            BindingKey::App(binding.app),
            BindingCapabilities::Operations(&binding.operation_ids),
        ),
        AppBinding::Folder(binding) => (
            BindingKey::Folder(binding.folder),
            BindingCapabilities::Folder,
        ),
        AppBinding::GatewayOperations(binding) => (
            BindingKey::GatewayApp(&binding.gateway_app),
            BindingCapabilities::Operations(&binding.operation_ids),
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
/// binds, the folder root it binds, or the gateway app it binds. The three
/// namespaces are disjoint by construction — a gateway app id that happens to
/// spell a local record's id is still a different key.
#[derive(PartialEq, Eq, Hash)]
enum BindingKey<'a> {
    App(ConnectedAppId),
    Folder(HostRootId),
    GatewayApp(&'a str),
}

/// One binding's capability list, by vocabulary, for the shared grammar check.
enum BindingCapabilities<'a> {
    Operations(&'a [String]),
    Folder,
}

/// The binding grammar shared by manifests and grants: no duplicate connected
/// apps, folders, or gateway apps (one binding per record, root, or gateway
/// app, whichever vocabulary), and every operation id within the ingest
/// module's `operationId` grammar. A folder binding has no list to validate —
/// its access level is closed by type.
///
/// The grammar here is structural only; the catalog membership check for
/// operation ids belongs to the host layers that resolve ids (the
/// `create_app` door, grant computation, the invoke gate), and for a gateway
/// app the gateway resolves both the app and its catalog.
fn validate_binding_set<'a>(
    bindings: impl Iterator<Item = (BindingKey<'a>, BindingCapabilities<'a>)>,
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
            BindingKey::GatewayApp(gateway_app) => {
                // The id is the gateway's, so it is bounded and kept
                // printable rather than parsed: Tidebreak never interprets it.
                if gateway_app.is_empty()
                    || gateway_app.len() > MAX_GATEWAY_APP_ID_BYTES
                    || !gateway_app.bytes().all(|byte| byte.is_ascii_graphic())
                {
                    return Err(format!(
                        "gateway app id {gateway_app:?} must be 1 to \
                         {MAX_GATEWAY_APP_ID_BYTES} bytes of printable, non-whitespace \
                         ASCII"
                    ));
                }
                if keys.contains(&key) {
                    return Err(format!("duplicate binding for gateway app {gateway_app}"));
                }
            }
        }
        keys.insert(key);
        match capabilities {
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
    fn manifests_enforce_the_name_grammar() {
        // Bindings may be empty: a static app pins no capabilities.
        assert!(validate_app_manifest(&AppManifest {
            name: "Static".into(),
            bindings: Vec::new(),
        })
        .is_ok());

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
        // One binding per connected app: the same record cannot be bound
        // twice.
        assert!(
            validate_app_manifest(&AppManifest {
                name: "Twice".into(),
                bindings: vec![
                    AppBinding::Operations(AppOperationsBinding {
                        app,
                        operation_ids: Vec::new()
                    }),
                    AppBinding::Operations(AppOperationsBinding {
                        app,
                        operation_ids: Vec::new()
                    }),
                ],
            })
            .is_err(),
            "duplicate connected-app bindings are refused"
        );
    }

    #[test]
    fn bindings_deserialize_untagged_and_closed() {
        use serde_json::json;

        let app = ConnectedAppId::new();
        let operations: AppBinding =
            serde_json::from_value(json!({ "app": app, "operation_ids": ["listIssues"] })).unwrap();
        assert!(matches!(operations, AppBinding::Operations(_)));
        // The retired tools vocabulary (#1332, removed in #1589), a binding
        // mixing vocabularies, any unknown field, and a bare app all match no
        // closed variant.
        for body in [
            json!({ "app": app, "tools": ["mcp__srv__viewer"] }),
            json!({ "app": app, "tools": [], "operation_ids": [] }),
            json!({ "app": app, "operation_ids": [], "extra": true }),
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
        // A pre-removal tools grant reads as unparseable, not as any variant.
        for body in [
            json!({ "app": app, "tools": ["mcp__srv__viewer"], "fingerprint": fingerprint }),
            json!({ "app": app, "tools": [], "operation_ids": [], "fingerprint": fingerprint }),
        ] {
            assert!(
                serde_json::from_value::<AppGrantBinding>(body.clone()).is_err(),
                "{body}"
            );
        }
    }

    /// The gateway vocabulary joins the same untagged, closed grammar: its
    /// shape parses to its own arm, mixing it with a local app-keyed shape
    /// matches nothing, its id lives in its own namespace for the duplicate
    /// check, and its grant twin round-trips with the pinned fingerprint.
    #[test]
    fn gateway_bindings_parse_closed_and_keep_their_own_namespace() {
        use serde_json::json;

        let app = ConnectedAppId::new();
        let gateway_app = "0f6d2f6a-1f0d-4a0e-9f6a-6f9d1d2f3a4b";
        let binding: AppBinding = serde_json::from_value(
            json!({ "gateway_app": gateway_app, "operation_ids": ["listIssues"] }),
        )
        .unwrap();
        let AppBinding::GatewayOperations(gateway) = &binding else {
            panic!("a gateway shape must parse as a gateway binding");
        };
        assert_eq!(gateway.gateway_app, gateway_app);
        // A gateway binding names no local record; it names the gateway's.
        assert!(binding.app().is_none());
        assert_eq!(binding.gateway_app(), Some(gateway_app));

        // Closed on every edge: a mixed shape, a bare id, and an unknown
        // field all refuse.
        for body in [
            json!({ "gateway_app": gateway_app, "app": app, "operation_ids": [] }),
            json!({ "gateway_app": gateway_app, "operation_ids": [], "extra": true }),
            json!({ "gateway_app": gateway_app }),
        ] {
            assert!(
                serde_json::from_value::<AppBinding>(body.clone()).is_err(),
                "{body}"
            );
        }

        // The id grammar is bounded and printable, and its operation ids obey
        // the same grammar the local operations arm does.
        let manifest = |gateway_app: &str, operation_ids: &[&str]| AppManifest {
            name: "Org issues".into(),
            bindings: vec![AppBinding::GatewayOperations(AppGatewayOperationsBinding {
                gateway_app: gateway_app.to_owned(),
                operation_ids: operation_ids.iter().map(|id| (*id).to_owned()).collect(),
            })],
        };
        assert!(validate_app_manifest(&manifest(gateway_app, &["listIssues"])).is_ok());
        for bad_id in ["", "has space", &"x".repeat(MAX_GATEWAY_APP_ID_BYTES + 1)] {
            assert!(
                validate_app_manifest(&manifest(bad_id, &[])).is_err(),
                "{bad_id:?}"
            );
        }
        assert!(validate_app_manifest(&manifest(gateway_app, &["bad id"])).is_err());
        assert!(
            validate_app_manifest(&AppManifest {
                name: "Twice".into(),
                bindings: vec![
                    AppBinding::GatewayOperations(AppGatewayOperationsBinding {
                        gateway_app: gateway_app.to_owned(),
                        operation_ids: Vec::new(),
                    }),
                    AppBinding::GatewayOperations(AppGatewayOperationsBinding {
                        gateway_app: gateway_app.to_owned(),
                        operation_ids: Vec::new(),
                    }),
                ],
            })
            .is_err(),
            "duplicate gateway-app bindings are refused"
        );
        // The namespaces are disjoint: a gateway id that spells a local
        // record's id binds a different thing, and the two coexist.
        assert!(validate_app_manifest(&AppManifest {
            name: "Both".into(),
            bindings: vec![
                AppBinding::Operations(AppOperationsBinding {
                    app,
                    operation_ids: vec!["listIssues".into()],
                }),
                AppBinding::GatewayOperations(AppGatewayOperationsBinding {
                    gateway_app: app.to_string(),
                    operation_ids: vec!["listIssues".into()],
                }),
            ],
        })
        .is_ok());

        // The grant twin round-trips with its pinned fingerprint, keeps the
        // untagged discrimination, and refuses the unpinned shape.
        let fingerprint = "ef".repeat(32);
        let granted: AppGrantBinding = serde_json::from_value(json!({
            "gateway_app": gateway_app,
            "operation_ids": ["listIssues"],
            "fingerprint": fingerprint,
        }))
        .unwrap();
        assert!(matches!(granted, AppGrantBinding::GatewayOperations(_)));
        assert!(granted.app().is_none());
        assert_eq!(granted.gateway_app(), Some(gateway_app));
        assert_eq!(granted.fingerprint(), [0xef; 32]);
        assert!(serde_json::from_value::<AppGrantBinding>(
            json!({ "gateway_app": gateway_app, "operation_ids": [] })
        )
        .is_err());
        // A local operations grant never reads as a gateway grant, whichever
        // way the ids happen to spell.
        let local: AppGrantBinding = serde_json::from_value(
            json!({ "app": app, "operation_ids": [], "fingerprint": fingerprint }),
        )
        .unwrap();
        assert!(local.gateway_app().is_none());
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
}
