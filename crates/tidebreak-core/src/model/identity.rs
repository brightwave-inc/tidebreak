use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::id::{HostRootId, ProjectId, RootAttachmentChangeId, SessionId};

/// Maximum number of host roots projected onto one project or conversation.
///
/// The host broker separately bounds and authorizes its live registry. This
/// product-side limit keeps API responses, turn snapshots, and future CAS
/// replacements predictably small.
pub const MAX_ROOT_ATTACHMENTS: usize = 32;

/// The durable owner key of a root aggregate (chat, project, document).
///
/// This is the storage-side identity of a principal (#853): the server maps
/// the authenticated principal onto an `OwnerId` and every owner-scoped store
/// query filters on it. The desktop profile has exactly one principal — the
/// person at the machine — whose key is [`OwnerId::local`]; named users on a
/// shared deployment map to distinct keys that can never collide with it.
///
/// The key is an identity label, not a secret: it lives in owner columns and
/// is kept greppable and log-safe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OwnerId(std::sync::Arc<str>);

impl OwnerId {
    /// The durable key of the single local-profile owner. Existing rows from
    /// before owner attribution are backfilled to this owner.
    pub const LOCAL: &'static str = "local";

    /// The one person at the machine on the desktop/local profile.
    #[must_use]
    pub fn local() -> Self {
        Self(Self::LOCAL.into())
    }

    /// Validate and intern an owner key: 1–96 visible ASCII characters.
    pub fn new(id: &str) -> crate::error::Result<Self> {
        let valid = !id.is_empty() && id.len() <= 96 && id.bytes().all(|b| b.is_ascii_graphic());
        if valid {
            Ok(Self(id.into()))
        } else {
            Err(crate::error::AgentError::Store(format!(
                "invalid owner id {id:?}: expected 1-96 visible ASCII characters"
            )))
        }
    }

    /// The exact durable key, as stored in owner columns.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the local-profile owner.
    #[must_use]
    pub fn is_local(&self) -> bool {
        &*self.0 == Self::LOCAL
    }
}

impl std::fmt::Display for OwnerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for OwnerId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OwnerId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(&raw).map_err(serde::de::Error::custom)
    }
}

/// Largest attachment revision represented exactly by every supported client.
///
/// JSON numbers become JavaScript `number` values in the desktop renderer, so
/// revisions stay within the integer-safe range instead of silently losing CAS
/// precision at the product boundary.
pub const MAX_ATTACHMENT_REVISION: i64 = 9_007_199_254_740_991;

/// Why a root appears in one conversation's exact ordered projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RootAttachmentOrigin {
    /// Snapshotted from project defaults when the conversation was created.
    ProjectDefault,
    /// Added specifically to this conversation by trusted native control.
    Conversation,
}

/// One pathless root in a conversation's exact ordered projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct ChatRootAttachment {
    /// Opaque broker root identity. This value grants no authority by itself.
    pub root_id: HostRootId,
    /// Product-level provenance for ordering and future management UI.
    pub origin: RootAttachmentOrigin,
}

/// Desired broker and product state for one durable root-attachment change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAttachmentChangeAction {
    /// Make the root available to this conversation.
    Attach,
    /// Remove the root only from this conversation.
    Detach,
}

/// Product identity that owns the broker grant used by an attachment change.
///
/// This is derived by the store while the chat and its projection are locked;
/// callers cannot choose it in [`BeginRootAttachmentChange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAttachmentSubjectKind {
    /// The root is owned by the chat's project.
    Project,
    /// The root is owned by this exact conversation.
    Conversation,
}

/// Durable phase of a product-side attachment state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAttachmentChangePhase {
    /// Product intent is durable and awaits broker reconciliation.
    AwaitingBroker,
    /// Broker reconciliation and the final product projection are durable.
    Completed,
    /// Broker reconciliation failed and the final product projection is durable.
    Failed,
}

/// Bounded transport-safe failure retained by an attachment change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootAttachmentChangeFailure {
    /// Stable machine-readable failure category.
    pub code: String,
    /// Bounded diagnostic message safe to retain in product storage.
    pub message: String,
    /// Whether trusted native control may offer an explicit retry.
    pub retryable: bool,
}

impl RootAttachmentChangeFailure {
    /// Maximum UTF-8 bytes in a stable failure code.
    pub const MAX_CODE_LEN: usize = 64;
    /// Maximum UTF-8 bytes in a retained diagnostic message.
    pub const MAX_MESSAGE_LEN: usize = 256;

    /// Validate the bounded failure payload before persistence.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.code.is_empty() {
            return Err("root attachment failure code must not be empty");
        }
        if self.code.len() > Self::MAX_CODE_LEN {
            return Err("root attachment failure code exceeds the supported limit");
        }
        if self.code.contains('\0') {
            return Err("root attachment failure code contains a null byte");
        }
        if self.message.is_empty() {
            return Err("root attachment failure message must not be empty");
        }
        if self.message.len() > Self::MAX_MESSAGE_LEN {
            return Err("root attachment failure message exceeds the supported limit");
        }
        if self.message.contains('\0') {
            return Err("root attachment failure message contains a null byte");
        }
        Ok(())
    }
}

/// Caller-controlled identity and intent for beginning one attachment change.
///
/// Subject ownership, projection provenance, and projection position are
/// intentionally absent. The store derives them atomically from authoritative
/// chat state rather than trusting a native or HTTP caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginRootAttachmentChange {
    /// Stable idempotency identity, also used as the broker operation UUID.
    pub id: RootAttachmentChangeId,
    /// Conversation whose exact broker attachment changes.
    pub chat_id: SessionId,
    /// Stable native reconciler allowed to finish this work.
    pub executor_id: Uuid,
    /// Opaque root identity; possession grants no authority.
    pub root_id: HostRootId,
    /// Desired attachment state.
    pub action: RootAttachmentChangeAction,
    /// CAS fence observed by the caller.
    pub expected_attachment_revision: i64,
    /// Caller operation time retained at database microsecond precision as
    /// immutable request identity.
    pub created_at: DateTime<Utc>,
}

impl BeginRootAttachmentChange {
    /// Validate caller-controlled fields before beginning durable work.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.chat_id.as_uuid().is_nil() {
            return Err("root attachment change chat id must not be nil");
        }
        if self.executor_id.is_nil() {
            return Err("root attachment change executor id must not be nil");
        }
        if !(0..=MAX_ATTACHMENT_REVISION).contains(&self.expected_attachment_revision) {
            return Err("expected attachment revision is outside the supported range");
        }
        Ok(())
    }
}

/// Exact broker observation supplied when finishing an attachment change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RootAttachmentChangeTerminal {
    /// The broker durably applied or recovered the exact requested mutation.
    Completed {
        /// Whether that broker operation changed its live attachment set.
        broker_changed: bool,
        /// Broker attachment state observed by the terminal receipt.
        broker_currently_attached: bool,
    },
    /// The broker durably rejected or failed the exact requested mutation.
    Failed {
        /// Whether the failed broker operation reported changing live state.
        broker_changed: Option<bool>,
        /// Live attachment state when the broker could report it.
        broker_currently_attached: Option<bool>,
        /// Stable bounded failure retained for exact retries.
        failure: RootAttachmentChangeFailure,
    },
}

impl RootAttachmentChangeTerminal {
    /// Validate bounded terminal data before persistence.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Completed { .. } => Ok(()),
            Self::Failed { failure, .. } => failure.validate(),
        }
    }
}

/// Product-side durable state for one broker-backed attachment mutation.
///
/// The immutable subject and projection metadata are derived under the same
/// store lock as `before_revision` and the intent projection. Flat
/// optional terminal fields make exact state validation explicit at storage
/// boundaries and map directly to relational columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootAttachmentChange {
    pub id: RootAttachmentChangeId,
    pub chat_id: SessionId,
    pub executor_id: Uuid,
    pub root_id: HostRootId,
    pub action: RootAttachmentChangeAction,
    /// Broker grant subject derived from authoritative chat state.
    pub subject_kind: RootAttachmentSubjectKind,
    /// Project or conversation UUID paired with `subject_kind`.
    pub subject_id: Uuid,
    /// Projection provenance captured by the store, when the root existed.
    pub origin: Option<RootAttachmentOrigin>,
    /// Zero-based projection position captured by the store, when applicable.
    pub projection_position: Option<u32>,
    /// Whether the root appeared in the projection before this operation.
    pub projection_existed_before: bool,
    pub expected_revision: i64,
    /// Authoritative revision observed when begin committed.
    pub before_revision: i64,
    /// Revision after begin durably projected the operation's intent.
    pub intent_revision: i64,
    pub phase: RootAttachmentChangePhase,
    /// Final revision after completion or rollback; absent while awaiting broker.
    pub result_revision: Option<i64>,
    /// Whether the final projection differs from the projection before begin.
    pub projection_changed: Option<bool>,
    /// Historical broker mutation result, required for completed work and
    /// retained for failed work when the broker could report it.
    pub broker_changed: Option<bool>,
    /// Terminal broker attachment observation, required for completed work and
    /// retained for failed work when the broker could report it.
    pub broker_currently_attached: Option<bool>,
    /// Stable broker failure, present only for failed work.
    pub failure: Option<RootAttachmentChangeFailure>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl RootAttachmentChange {
    /// Validate identity, revision, projection, and terminal-state invariants.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.chat_id.as_uuid().is_nil() {
            return Err("root attachment change chat id must not be nil");
        }
        if self.executor_id.is_nil() {
            return Err("root attachment change executor id must not be nil");
        }
        if self.subject_id.is_nil() {
            return Err("root attachment change subject id must not be nil");
        }
        if self.subject_kind == RootAttachmentSubjectKind::Conversation
            && self.subject_id != *self.chat_id.as_uuid()
        {
            return Err("conversation attachment subject must match the chat");
        }
        for revision in [
            self.expected_revision,
            self.before_revision,
            self.intent_revision,
        ] {
            if !(0..=MAX_ATTACHMENT_REVISION).contains(&revision) {
                return Err("root attachment change revision is outside the supported range");
            }
        }
        if self
            .result_revision
            .is_some_and(|revision| !(0..=MAX_ATTACHMENT_REVISION).contains(&revision))
        {
            return Err("root attachment change result revision is outside the supported range");
        }
        if self.expected_revision != self.before_revision {
            return Err("root attachment change began from an unexpected revision");
        }
        if self.projection_position.is_some() != self.origin.is_some() {
            return Err("root attachment projection origin and position must appear together");
        }
        let projection_metadata_required =
            self.projection_existed_before || self.action == RootAttachmentChangeAction::Attach;
        if projection_metadata_required != self.origin.is_some() {
            return Err("root attachment prior projection metadata is inconsistent");
        }
        if self.action == RootAttachmentChangeAction::Attach
            && !self.projection_existed_before
            && self.origin != Some(RootAttachmentOrigin::Conversation)
        {
            return Err("a newly attached root must use conversation provenance");
        }
        if self
            .projection_position
            .is_some_and(|position| position as usize >= MAX_ROOT_ATTACHMENTS)
        {
            return Err("root attachment projection position exceeds the supported limit");
        }
        let intent_advanced =
            self.action == RootAttachmentChangeAction::Attach && !self.projection_existed_before;
        let terminal_may_advance = intent_advanced
            || (self.action == RootAttachmentChangeAction::Detach
                && self.projection_existed_before);
        let required_headroom = i64::from(intent_advanced) + i64::from(terminal_may_advance);
        if self.before_revision > MAX_ATTACHMENT_REVISION - required_headroom {
            return Err("root attachment change lacks reserved revision headroom");
        }
        let expected_intent_revision = self
            .before_revision
            .checked_add(i64::from(intent_advanced))
            .ok_or("root attachment intent revision overflowed")?;
        if self.intent_revision != expected_intent_revision {
            return Err("root attachment intent revision is inconsistent");
        }

        match self.phase {
            RootAttachmentChangePhase::AwaitingBroker => {
                if self.result_revision.is_some()
                    || self.projection_changed.is_some()
                    || self.broker_changed.is_some()
                    || self.broker_currently_attached.is_some()
                    || self.failure.is_some()
                    || self.finished_at.is_some()
                {
                    return Err("awaiting root attachment change has terminal fields");
                }
            }
            RootAttachmentChangePhase::Completed => {
                if self.result_revision.is_none()
                    || self.projection_changed.is_none()
                    || self.broker_changed.is_none()
                    || self.broker_currently_attached.is_none()
                    || self.failure.is_some()
                    || self.finished_at.is_none()
                {
                    return Err("completed root attachment change has invalid terminal fields");
                }
            }
            RootAttachmentChangePhase::Failed => {
                if self.result_revision.is_none()
                    || self.projection_changed.is_none()
                    || self.failure.is_none()
                    || self.finished_at.is_none()
                {
                    return Err("failed root attachment change has invalid terminal fields");
                }
                self.failure.as_ref().expect("checked above").validate()?;
            }
        }
        if self
            .finished_at
            .is_some_and(|finished_at| finished_at < self.created_at)
        {
            return Err("root attachment change finish time precedes creation");
        }
        if self.phase != RootAttachmentChangePhase::AwaitingBroker {
            let completed = self.phase == RootAttachmentChangePhase::Completed;
            let terminal_removal = (completed
                && self.action == RootAttachmentChangeAction::Detach
                && self.projection_existed_before)
                || (!completed && intent_advanced);
            let expected_result_revision = self
                .intent_revision
                .checked_add(i64::from(terminal_removal))
                .ok_or("root attachment result revision overflowed")?;
            let desired_attached = self.action == RootAttachmentChangeAction::Attach;
            let expected_projection_changed =
                completed && self.projection_existed_before != desired_attached;
            if self.result_revision != Some(expected_result_revision)
                || self.projection_changed != Some(expected_projection_changed)
            {
                return Err("root attachment terminal projection metadata is inconsistent");
            }
            if completed && self.broker_currently_attached != Some(desired_attached) {
                return Err("completed root attachment change contradicts broker state");
            }
            if !completed
                && self
                    .broker_currently_attached
                    .is_some_and(|attached| attached == desired_attached)
            {
                return Err("failed root attachment change contradicts broker state");
            }
        }
        Ok(())
    }
}

/// An optional grouping of chats that share project context and a document
/// corpus. A chat may belong to a project or stand alone — unlike some designs
/// that make a project mandatory, Tidebreak keeps loose, projectless chats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct Project {
    /// Stable identifier.
    pub id: ProjectId,
    /// Human-facing title.
    pub title: Option<String>,
    /// CAS revision of the ordered root projection.
    pub attachment_revision: i64,
    /// Ordered opaque root defaults for conversations created in this project.
    /// These ids are product state, never host authorization.
    pub root_attachments: Vec<HostRootId>,
    /// When the project was created.
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    use crate::id::{HostRootId, RootAttachmentChangeId, SessionId};

    #[test]
    fn root_attachment_change_validation_enforces_derived_and_terminal_shape() {
        let chat_id = SessionId::new();
        let mut change = RootAttachmentChange {
            id: RootAttachmentChangeId::new(),
            chat_id,
            executor_id: Uuid::new_v4(),
            root_id: HostRootId::from_uuid(Uuid::new_v4()).unwrap(),
            action: RootAttachmentChangeAction::Attach,
            subject_kind: RootAttachmentSubjectKind::Conversation,
            subject_id: *chat_id.as_uuid(),
            origin: Some(RootAttachmentOrigin::Conversation),
            projection_position: Some(0),
            projection_existed_before: false,
            expected_revision: 0,
            before_revision: 0,
            intent_revision: 1,
            phase: RootAttachmentChangePhase::AwaitingBroker,
            result_revision: None,
            projection_changed: None,
            broker_changed: None,
            broker_currently_attached: None,
            failure: None,
            created_at: Utc::now(),
            finished_at: None,
        };
        assert_eq!(change.validate(), Ok(()));

        change.subject_id = Uuid::new_v4();
        assert!(change.validate().is_err());
        change.subject_id = *chat_id.as_uuid();
        change.intent_revision = 0;
        assert!(change.validate().is_err());
        change.intent_revision = 1;
        change.expected_revision = MAX_ATTACHMENT_REVISION - 1;
        change.before_revision = MAX_ATTACHMENT_REVISION - 1;
        change.intent_revision = MAX_ATTACHMENT_REVISION;
        assert!(change.validate().is_err());
        change.expected_revision = 0;
        change.before_revision = 0;
        change.intent_revision = 1;
        change.phase = RootAttachmentChangePhase::Completed;
        assert!(change.validate().is_err());
        change.result_revision = Some(1);
        change.projection_changed = Some(true);
        change.broker_changed = Some(true);
        change.broker_currently_attached = Some(true);
        change.finished_at = Some(change.created_at);
        assert_eq!(change.validate(), Ok(()));
        change.result_revision = Some(2);
        assert!(change.validate().is_err());
    }

    #[test]
    fn root_attachment_failure_is_bounded_by_utf8_bytes() {
        let valid = RootAttachmentChangeFailure {
            code: "broker_denied".into(),
            message: "root attachment was denied".into(),
            retryable: false,
        };
        assert_eq!(valid.validate(), Ok(()));

        let mut contains_null = valid.clone();
        contains_null.code.push('\0');
        assert!(contains_null.validate().is_err());
        contains_null = valid.clone();
        contains_null.message.push('\0');
        assert!(contains_null.validate().is_err());

        let mut oversized = valid;
        oversized.message = "é".repeat(RootAttachmentChangeFailure::MAX_MESSAGE_LEN / 2 + 1);
        assert!(oversized.validate().is_err());
    }
}
