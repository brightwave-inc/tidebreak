//! Strongly-typed identifiers.
//!
//! Every entity gets its own newtype over a UUID so the compiler stops us from,
//! say, passing a [`TurnId`] where a [`ChatId`] is expected. All ids
//! serialize transparently (as the bare UUID string), so on the wire and in the
//! `Store` they are indistinguishable from a plain UUID.
//!
//! Identity is minted only by [`id_type`]'s `new`, a typed `derive` /
//! `from_uuid`, or an explicit `From<Uuid>`. There is no `Default`: a
//! `..Default::default()` on a struct that happens to contain an id would
//! otherwise invent a durable identity as a side effect.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;
use uuid::Uuid;

/// Declares a UUID-backed identifier newtype with the common impls.
macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a fresh, random identifier.
            ///
            /// Not `Default`: a `..Default::default()` on a struct that
            /// happens to contain an id would invent a durable identity.
            #[must_use]
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Borrow the underlying UUID.
            #[must_use]
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }
    };
}

id_type!(
    /// Identifies a project: an optional grouping a chat may belong to.
    ProjectId
);

id_type!(
    /// Identifies one exact document chunk span.
    ChunkId
);

id_type!(
    /// Opaque renderer identity for one assistant-message citation.
    AssistantCitationId
);

id_type!(
    /// Opaque renderer identity for one output-revision citation.
    OutputCitationId
);

impl AssistantCitationId {
    const NAMESPACE: Uuid = Uuid::from_u128(0x2b74_9531_8d6e_4e11_a80b_8f50_413e_927c);

    /// Derive one stable citation identity from its message and one-based ordinal.
    #[must_use]
    pub fn derive(message_id: MessageId, ordinal: u16) -> Self {
        let message_namespace = Uuid::new_v5(&Self::NAMESPACE, message_id.as_uuid().as_bytes());
        Self(Uuid::new_v5(
            &message_namespace,
            ordinal.to_string().as_bytes(),
        ))
    }
}

impl OutputCitationId {
    const NAMESPACE: Uuid = Uuid::from_u128(0x9c4e_74f0_3198_40bd_8acd_3e09_83b5_c6d7);

    /// Derive one stable citation identity from its revision and one-based ordinal.
    #[must_use]
    pub fn derive(revision_id: OutputRevisionId, ordinal: u16) -> Self {
        let revision_namespace = Uuid::new_v5(&Self::NAMESPACE, revision_id.as_uuid().as_bytes());
        Self(Uuid::new_v5(
            &revision_namespace,
            ordinal.to_string().as_bytes(),
        ))
    }
}

impl ChunkId {
    const NAMESPACE: Uuid = Uuid::from_u128(0x9f8d_2c31_5b47_4e6a_a1c2_d3e4_f506_1728);

    /// Derive the stable chunk identity for one document byte span.
    #[must_use]
    pub fn derive(document_id: DocumentId, start: usize, end: usize) -> Self {
        let per_document = Uuid::new_v5(&Self::NAMESPACE, document_id.as_uuid().as_bytes());
        Self(Uuid::new_v5(
            &per_document,
            format!("{start}:{end}").as_bytes(),
        ))
    }
}

/// Opaque identifier for a folder registered with a host broker.
///
/// This is product projection data, not authority: possession of an id never
/// grants access to the corresponding host path. The broker independently
/// validates live attachments, consent, capabilities, and revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[serde(transparent)]
pub struct HostRootId(Uuid);

impl HostRootId {
    /// Build a root id from its non-nil wire UUID.
    pub fn from_uuid(uuid: Uuid) -> Result<Self, HostRootIdError> {
        if uuid.is_nil() {
            Err(HostRootIdError::Nil)
        } else {
            Ok(Self(uuid))
        }
    }

    /// Borrow the underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl<'de> Deserialize<'de> for HostRootId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let uuid = Uuid::deserialize(deserializer)?;
        Self::from_uuid(uuid).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for HostRootId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for HostRootId {
    type Err = HostRootIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_uuid(Uuid::parse_str(value)?)
    }
}

/// Invalid opaque host-root identity.
#[derive(Debug, Error)]
pub enum HostRootIdError {
    /// The value is not a UUID.
    #[error("invalid host root id: {0}")]
    InvalidUuid(#[from] uuid::Error),
    /// Nil is reserved and never identifies a broker root.
    #[error("host root id must not be nil")]
    Nil,
}
id_type!(
    /// Identifies an authoritative source document.
    ///
    /// Usually minted fresh with [`DocumentId::new`], but [`DocumentId::derive`]
    /// preserves the existing stable URI identity used by source ingestion.
    DocumentId
);
impl DocumentId {
    /// Namespace UUID for URI-derived document ids. This value is part of the
    /// persisted identity contract and must remain stable.
    const NAMESPACE: Uuid = Uuid::from_u128(0x1d0c_7a44_9e21_4b83_bc55_6677_8899_aabb);
    /// Separate namespace for identities derived from immutable source content.
    const CONTENT_NAMESPACE: Uuid = Uuid::from_u128(0x6e85_4dfa_94e0_45bb_a48a_65f7_25b9_3056);

    /// Derive a legacy stable id from only a source URI.
    ///
    /// New conversation-owned documents must use [`DocumentId::derive_for_chat`]
    /// so the same URI can be attached to more than one conversation.
    #[must_use]
    pub fn derive(uri: &str) -> Self {
        Self(Uuid::new_v5(&Self::NAMESPACE, uri.as_bytes()))
    }

    /// Derive a stable id from a project and source URI.
    #[must_use]
    pub fn derive_for_project(project_id: ProjectId, uri: &str) -> Self {
        let project_namespace = Uuid::new_v5(&Self::NAMESPACE, project_id.as_uuid().as_bytes());
        Self(Uuid::new_v5(&project_namespace, uri.as_bytes()))
    }

    /// Derive a stable id from a conversation and source URI.
    #[must_use]
    pub fn derive_for_chat(chat_id: ChatId, uri: &str) -> Self {
        let chat_namespace = Uuid::new_v5(&Self::NAMESPACE, chat_id.as_uuid().as_bytes());
        Self(Uuid::new_v5(&chat_namespace, uri.as_bytes()))
    }

    /// Derive a stable conversation-local id from an exact source digest.
    ///
    /// This keeps a content re-import idempotent without making one chat's
    /// document record own the same source in every other conversation.
    #[must_use]
    pub fn derive_for_chat_content(chat_id: ChatId, sha256: [u8; 32]) -> Self {
        let chat_namespace = Uuid::new_v5(&Self::CONTENT_NAMESPACE, chat_id.as_uuid().as_bytes());
        Self(Uuid::new_v5(&chat_namespace, &sha256))
    }
}
id_type!(
    /// Identifies a persistent conversation.
    ChatId
);
id_type!(
    /// Identifies one durable foreground or sandboxed background agent run.
    AgentRunId
);

impl AgentRunId {
    /// Namespace for the one foreground coordinator derived for each chat.
    const FOREGROUND_NAMESPACE: Uuid = Uuid::from_u128(0x38fd_6a64_b02f_4e91_a60c_7173_9af0_5b21);
    /// Namespace for a sandbox child derived from its exact model tool call.
    const SANDBOX_SPAWN_NAMESPACE: Uuid =
        Uuid::from_u128(0xb4b6_2a8f_6f85_4e0e_9e53_443a_4e69_a217);

    /// Derive the stable foreground coordinator identity for a chat.
    #[must_use]
    pub fn foreground_for_chat(chat_id: ChatId) -> Self {
        Self(Uuid::new_v5(
            &Self::FOREGROUND_NAMESPACE,
            chat_id.as_uuid().as_bytes(),
        ))
    }

    /// Derive the stable child identity for one exact model tool call.
    #[must_use]
    pub fn sandbox_for_spawn_call(call_id: CallId) -> Self {
        Self(Uuid::new_v5(
            &Self::SANDBOX_SPAWN_NAMESPACE,
            call_id.as_uuid().as_bytes(),
        ))
    }
}

/// Stable product and broker idempotency identity for one root-attachment change.
///
/// The same UUID is used when reconciling the change with the host broker. Nil
/// is reserved so a missing operation identity cannot become durable work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RootAttachmentChangeId(Uuid);

impl RootAttachmentChangeId {
    /// Generate a fresh change identity.
    ///
    /// Not `Default`: see [`id_type`].
    #[must_use]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Build a change identity from a non-nil UUID.
    pub fn from_uuid(uuid: Uuid) -> Result<Self, RootAttachmentChangeIdError> {
        if uuid.is_nil() {
            Err(RootAttachmentChangeIdError::Nil)
        } else {
            Ok(Self(uuid))
        }
    }

    /// Borrow the UUID also used as the broker operation identity.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RootAttachmentChangeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let uuid = Uuid::deserialize(deserializer)?;
        Self::from_uuid(uuid).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for RootAttachmentChangeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for RootAttachmentChangeId {
    type Err = RootAttachmentChangeIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_uuid(Uuid::parse_str(value)?)
    }
}

/// Invalid durable root-attachment change identity.
#[derive(Debug, Error)]
pub enum RootAttachmentChangeIdError {
    /// The value is not a UUID.
    #[error("invalid root attachment change id: {0}")]
    InvalidUuid(#[from] uuid::Error),
    /// Nil is reserved and never identifies durable attachment work.
    #[error("root attachment change id must not be nil")]
    Nil,
}
id_type!(
    /// Identifies a persisted message within a chat.
    MessageId
);

impl MessageId {
    /// Namespace for renderer-only compaction divider identities.
    const COMPACTION_DIVIDER_NAMESPACE: Uuid =
        Uuid::from_u128(0x6a1c_e8f2_4b9d_4a70_9e3f_12d5_87a0_bc41);

    /// Stable synthetic id for the transcript divider after a compaction boundary.
    ///
    /// Not a durable message row — only projected into the chat messages API so
    /// clients can render one "compacted conversation" marker.
    #[must_use]
    pub fn compaction_divider(source_message_id: MessageId) -> Self {
        Self(Uuid::new_v5(
            &Self::COMPACTION_DIVIDER_NAMESPACE,
            source_message_id.as_uuid().as_bytes(),
        ))
    }
}

id_type!(
    /// Identifies one turn: a single user input through to the final answer.
    TurnId
);
id_type!(
    /// Identifies one idempotent steering instruction for a live turn.
    TurnSteerId
);
id_type!(
    /// Identifies one step within a turn: a single LLM call and its tools.
    StepId
);
id_type!(
    /// Identifies one tool call, stable across its request/approval/result.
    CallId
);
id_type!(
    /// Identifies one conversation-owned output across all of its revisions.
    ///
    /// This is the durable handle a model, renderer, or export names. It is
    /// deliberately opaque: possession of an id is not authority, and it never
    /// encodes a filename or a host path.
    OutputId
);

impl OutputId {
    const NAMESPACE: Uuid = Uuid::from_u128(0x5500_62d4_2528_4cc6_90f8_a788_e119_bf36);

    /// Derive the retry-stable output identity for one canonical tool call.
    #[must_use]
    pub fn for_call(call_id: CallId) -> Self {
        Self(Uuid::new_v5(&Self::NAMESPACE, call_id.as_uuid().as_bytes()))
    }

    /// Derive the retry-stable output identity for one artifact filename
    /// published by one canonical tool call.
    ///
    /// An execution call can publish several files at once, so unlike
    /// [`OutputId::for_call`] the identity covers the (call, filename) pair: a
    /// retried call lands each file on the same output instead of forking new
    /// records, and two files from one call never collide.
    #[must_use]
    pub fn for_call_artifact(call_id: CallId, filename: &str) -> Self {
        Self(Uuid::new_v5(
            &Self::NAMESPACE,
            &call_artifact_name(call_id, filename),
        ))
    }
}

/// The stable UUIDv5 name for a (call, filename) artifact identity.
fn call_artifact_name(call_id: CallId, filename: &str) -> Vec<u8> {
    let mut name = call_id.as_uuid().as_bytes().to_vec();
    name.extend_from_slice(filename.as_bytes());
    name
}

id_type!(
    /// Identifies one immutable revision of an output.
    OutputRevisionId
);

impl OutputRevisionId {
    const NAMESPACE: Uuid = Uuid::from_u128(0x72cb_0277_5a3c_45ee_bda8_4353_4f74_feb2);

    /// Derive the retry-stable revision identity for one canonical tool call.
    #[must_use]
    pub fn for_call(call_id: CallId) -> Self {
        Self(Uuid::new_v5(&Self::NAMESPACE, call_id.as_uuid().as_bytes()))
    }

    /// Derive the retry-stable revision identity for one artifact filename
    /// published by one canonical tool call. See
    /// [`OutputId::for_call_artifact`].
    #[must_use]
    pub fn for_call_artifact(call_id: CallId, filename: &str) -> Self {
        Self(Uuid::new_v5(
            &Self::NAMESPACE,
            &call_artifact_name(call_id, filename),
        ))
    }

    /// Derive the retry-stable revision identity for restoring one target
    /// revision at one exact history position.
    ///
    /// Restoring appends rather than rewinds, so the new revision's identity
    /// covers the (target revision, new ordinal) pair: retrying an ambiguous
    /// restore from the same state lands on the same revision, while restoring
    /// the same target again later appends a distinct one.
    #[must_use]
    pub fn for_restore(target: OutputRevisionId, ordinal: u32) -> Self {
        let mut name = target.as_uuid().as_bytes().to_vec();
        name.extend_from_slice(&ordinal.to_be_bytes());
        Self(Uuid::new_v5(&Self::NAMESPACE, &name))
    }

    /// Derive the retry-stable revision identity for one user edit.
    ///
    /// An edit is fully described by what it started from and what it produced,
    /// so the identity covers the (base revision, content digest) pair. Retrying
    /// an ambiguous save lands on the same revision instead of appending the
    /// same text twice, while a later edit — necessarily from a different base —
    /// gets its own identity even if it restores earlier wording.
    #[must_use]
    pub fn for_user_edit(base: OutputRevisionId, sha256: &[u8; 32]) -> Self {
        let mut name = base.as_uuid().as_bytes().to_vec();
        name.extend_from_slice(sha256);
        Self(Uuid::new_v5(&Self::NAMESPACE, &name))
    }
}

id_type!(
    /// Identifies one profile-scoped local app across all of its revisions.
    ///
    /// Like [`OutputId`] this is a durable opaque handle: possession is not
    /// authority, and it never encodes a name or a host path. Unlike an
    /// output, an app has no owning conversation — the profile owns it.
    AppId
);

impl AppId {
    const NAMESPACE: Uuid = Uuid::from_u128(0xffa3_faf3_1345_4d93_a843_5f92_c996_7331);

    /// Derive the retry-stable app identity for one canonical tool call.
    ///
    /// `create_app` mints identity the way the output tools do: from the
    /// durable call id, so an ambiguous store response retried by the model
    /// lands on the same app rather than forking a second record.
    #[must_use]
    pub fn for_call(call_id: CallId) -> Self {
        Self(Uuid::new_v5(&Self::NAMESPACE, call_id.as_uuid().as_bytes()))
    }
}

id_type!(
    /// Identifies one profile-scoped connected app — an outside integration
    /// (an MCP server, a REST API) a profile can reach.
    ///
    /// App-keyed manifest bindings and grants name this identity rather than a
    /// raw server namespace, so consent follows the record even when display
    /// names or namespaces change around it.
    ConnectedAppId
);

// Ordered so fingerprint maps can key by record id; the macro's derives stop
// at `Eq` because no other id is used as a map key in sorted collections.
impl PartialOrd for ConnectedAppId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ConnectedAppId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl ConnectedAppId {
    const NAMESPACE: Uuid = Uuid::from_u128(0x1f4c_9a70_60fd_4b2e_9a41_c2d1_08e7_5d23);

    /// Derive the boot-stable identity for a server selected by the legacy
    /// boot file (`TIDEBREAK_MCP_CONFIG`), which configures servers without
    /// persisting records. Deriving from the configured name keeps app grants
    /// valid across restarts of a boot-file profile; a persisted record keeps
    /// whatever id it was created with.
    #[must_use]
    pub fn for_boot_server(name: &str) -> Self {
        Self(Uuid::new_v5(&Self::NAMESPACE, name.as_bytes()))
    }
}

/// Schema for the one id type the model writes: `create_app` manifests bind
/// connected apps by id, so the argument schema must carry it.
impl schemars::JsonSchema for ConnectedAppId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ConnectedAppId".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Uuid as schemars::JsonSchema>::json_schema(generator)
    }
}

/// Schema for the other id the model writes: `create_app` manifests bind
/// connected folders by broker root id.
impl schemars::JsonSchema for HostRootId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "HostRootId".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Uuid as schemars::JsonSchema>::json_schema(generator)
    }
}

id_type!(
    /// Identifies one immutable revision of a local app.
    AppRevisionId
);

impl AppRevisionId {
    const NAMESPACE: Uuid = Uuid::from_u128(0xb60b_2034_fb5a_4651_b284_646f_f59b_a069);

    /// Derive the retry-stable revision identity for one canonical tool call.
    ///
    /// One call publishes exactly one revision — of a new app or of the app it
    /// appends to — so the call id alone is the whole identity.
    #[must_use]
    pub fn for_call(call_id: CallId) -> Self {
        Self(Uuid::new_v5(&Self::NAMESPACE, call_id.as_uuid().as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_agent_run_ids_are_stable_and_chat_scoped() {
        let first_chat = ChatId::new();
        let second_chat = ChatId::new();
        assert_eq!(
            AgentRunId::foreground_for_chat(first_chat),
            AgentRunId::foreground_for_chat(first_chat)
        );
        assert_ne!(
            AgentRunId::foreground_for_chat(first_chat),
            AgentRunId::foreground_for_chat(second_chat)
        );
    }

    #[test]
    fn roundtrips_through_string_and_json() {
        let id = ChatId::new();
        assert_eq!(id.to_string().parse::<ChatId>().unwrap(), id);

        let json = serde_json::to_string(&id).unwrap();
        // Transparent: serializes as the bare quoted UUID, no wrapper.
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<ChatId>(&json).unwrap(), id);
    }

    #[test]
    fn host_root_ids_reject_nil_and_roundtrip() {
        let uuid = Uuid::new_v4();
        let id = HostRootId::from_uuid(uuid).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<HostRootId>(&json).unwrap(), id);
        assert!(HostRootId::from_uuid(Uuid::nil()).is_err());
        assert!(
            serde_json::from_str::<HostRootId>("\"00000000-0000-0000-0000-000000000000\"").is_err()
        );
    }

    #[test]
    fn root_attachment_change_ids_reject_nil_and_roundtrip() {
        let uuid = Uuid::new_v4();
        let id = RootAttachmentChangeId::from_uuid(uuid).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(
            serde_json::from_str::<RootAttachmentChangeId>(&json).unwrap(),
            id
        );
        assert!(RootAttachmentChangeId::from_uuid(Uuid::nil()).is_err());
        assert!(serde_json::from_str::<RootAttachmentChangeId>(
            "\"00000000-0000-0000-0000-000000000000\""
        )
        .is_err());
    }

    #[test]
    fn document_uri_derivation_is_stable() {
        assert_eq!(
            DocumentId::derive("file:///a.txt"),
            DocumentId::derive("file:///a.txt")
        );
        assert_ne!(
            DocumentId::derive("file:///a.txt"),
            DocumentId::derive("file:///b.txt")
        );

        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        assert_eq!(
            DocumentId::derive_for_project(project_a, "file:///a.txt"),
            DocumentId::derive_for_project(project_a, "file:///a.txt")
        );
        assert_ne!(
            DocumentId::derive_for_project(project_a, "file:///a.txt"),
            DocumentId::derive_for_project(project_b, "file:///a.txt")
        );
        assert_ne!(
            DocumentId::derive_for_project(project_a, "file:///a.txt"),
            DocumentId::derive("file:///a.txt")
        );

        let chat_a = ChatId::new();
        let chat_b = ChatId::new();
        assert_eq!(
            DocumentId::derive_for_chat(chat_a, "file:///a.txt"),
            DocumentId::derive_for_chat(chat_a, "file:///a.txt")
        );
        assert_ne!(
            DocumentId::derive_for_chat(chat_a, "file:///a.txt"),
            DocumentId::derive_for_chat(chat_b, "file:///a.txt")
        );
        assert_ne!(
            DocumentId::derive_for_chat(chat_a, "file:///a.txt"),
            DocumentId::derive("file:///a.txt")
        );

        let digest = [0x55; 32];
        assert_eq!(
            DocumentId::derive_for_chat_content(chat_a, digest),
            DocumentId::derive_for_chat_content(chat_a, digest)
        );
        assert_ne!(
            DocumentId::derive_for_chat_content(chat_a, digest),
            DocumentId::derive_for_chat_content(chat_b, digest)
        );
        assert_ne!(
            DocumentId::derive_for_chat_content(chat_a, digest),
            DocumentId::derive_for_chat_content(chat_a, [0x56; 32])
        );
    }
}
