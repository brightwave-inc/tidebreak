//! Deny-by-default capability grants and conversation attachments.
//!
//! These are persistence and protocol-safe value types. The matcher in this
//! module exists only in tests to specify the intended grant and attachment
//! intersection. A later broker operation layer must derive the capability and
//! resource from a typed request, reauthorize, and perform the effect without
//! handing callers a reusable authorization token.

use chrono::{DateTime, Utc};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[cfg(test)]
use crate::ExecutionContext;
use crate::{GrantId, GrantSubject, RelativePath, RootId};

/// A class of host action guarded independently by the broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Capability {
    /// Discover safe summaries of roots connected to this subject.
    ListRoots,
    /// List directories and read file bytes within a connected root.
    ReadFiles,
    /// Create or explicitly approved replacement of files in a connected root.
    WriteFiles,
    /// Expose a connected root to model-authored commands.
    ///
    /// This is deliberately separate from [`Capability::ReadFiles`]: reading a
    /// named file on the user's behalf and handing a whole folder to commands
    /// the model wrote are different amounts of trust, even though the second
    /// cannot see more than the first. It is only ever additional — exec reach
    /// is authorized on top of read, never instead of it — so revoking it
    /// leaves the folder readable, and revoking read takes exec with it.
    ExecuteCommands,
    /// Capture the screen. Scoped to the whole display ([`Scope::Screen`]) or
    /// to one app ([`Scope::App`]).
    CaptureScreen,
    /// Read an app's accessibility tree (on-screen text, element roles, bounds).
    /// Scoped to one app ([`Scope::App`]).
    ReadAppContent,
    /// Drive an app: click, type, press keys, scroll, focus. The highest-trust
    /// computer-use capability; scoped to one app ([`Scope::App`]). Granting it
    /// implies [`Capability::ReadAppContent`] and [`Capability::CaptureScreen`]
    /// for the same app.
    ControlApp,
}

impl Capability {
    /// Whether holding `granted` satisfies a request for `requested` at the
    /// same scope.
    ///
    /// Reflexive, plus the computer-use implication chain: control implies both
    /// reads, and the two read capabilities imply each other. Implication is
    /// one-directional from control to reads — a read grant never confers
    /// control. Folder capabilities imply only themselves.
    ///
    /// This is the seam the broker's operation dispatch consults when it
    /// authorizes a computer-use request against the live grants; the folder
    /// `authorize` path matches capabilities exactly and does not use it.
    pub(crate) fn implies(granted: Capability, requested: Capability) -> bool {
        use Capability::{CaptureScreen, ControlApp, ReadAppContent};
        if granted == requested {
            return true;
        }
        match (granted, requested) {
            (ControlApp, ReadAppContent | CaptureScreen) => true,
            (ReadAppContent, CaptureScreen) | (CaptureScreen, ReadAppContent) => true,
            _ => false,
        }
    }
}

/// Resource scope covered by one grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Scope {
    /// A subject-wide action that touches no root, such as listing roots.
    Subject,
    /// An entire connected root.
    Root { root_id: RootId },
    /// One non-root subtree within a connected root.
    PathSubtree {
        root_id: RootId,
        relative: RelativePath,
    },
    /// One application, named by its macOS bundle id (e.g. "com.apple.Notes").
    /// The scope for `ReadAppContent` and `ControlApp`, and one of the two
    /// scopes for `CaptureScreen`.
    App { bundle_id: String },
    /// The whole display. The scope for a full-screen `CaptureScreen` grant.
    Screen,
}

/// How the user expressed consent for a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsentMethod {
    /// The user chose a folder through a trusted native picker.
    FolderPicker,
    /// The user approved a capability in an explicit permission dialog.
    PermissionDialog,
    /// A local operator deliberately provisioned a headless installation.
    OperatorConfig,
    /// A state migration named reach an existing grant already conveyed.
    ///
    /// No user interaction produced this record. It exists so a grant that was
    /// split out of a broader one keeps an honest provenance instead of
    /// borrowing the trusted interaction behind its source, and carries that
    /// source's timestamp rather than claiming a fresh approval.
    CarriedForward,
}

/// Auditable evidence attached to a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentRecord {
    method: ConsentMethod,
    granted_at: DateTime<Utc>,
}

impl ConsentRecord {
    pub(crate) const fn new(method: ConsentMethod, granted_at: DateTime<Utc>) -> Self {
        Self { method, granted_at }
    }

    /// Trusted interaction through which consent was captured.
    pub const fn method(&self) -> ConsentMethod {
        self.method
    }

    /// Host-stamped time at which the user granted access.
    pub const fn granted_at(&self) -> DateTime<Utc> {
        self.granted_at
    }
}

/// A persisted authorization for one exact product subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Grant {
    id: GrantId,
    subject: GrantSubject,
    capability: Capability,
    scope: Scope,
    consent: ConsentRecord,
}

impl Grant {
    pub(crate) fn from_consent(
        id: GrantId,
        subject: GrantSubject,
        capability: Capability,
        scope: Scope,
        consent: ConsentRecord,
    ) -> Result<Self, GrantError> {
        validate_capability_scope(capability, &scope)?;
        Ok(Self {
            id,
            subject,
            capability,
            scope,
            consent,
        })
    }

    /// Stable identity recorded in operation receipts and audit entries.
    pub const fn id(&self) -> GrantId {
        self.id
    }

    /// Project or conversation to which the standing consent belongs.
    pub const fn subject(&self) -> GrantSubject {
        self.subject
    }

    /// Action class the user allowed.
    pub const fn capability(&self) -> Capability {
        self.capability
    }

    /// Root or subtree in which the action is allowed.
    pub const fn scope(&self) -> &Scope {
        &self.scope
    }

    /// How and when the grant was created.
    pub const fn consent(&self) -> &ConsentRecord {
        &self.consent
    }
}

impl<'de> Deserialize<'de> for Grant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireGrant {
            id: GrantId,
            subject: GrantSubject,
            capability: Capability,
            scope: Scope,
            consent: ConsentRecord,
        }

        let wire = WireGrant::deserialize(deserializer)?;
        Self::from_consent(
            wire.id,
            wire.subject,
            wire.capability,
            wire.scope,
            wire.consent,
        )
        .map_err(D::Error::custom)
    }
}

/// A root explicitly attached to one conversation.
///
/// Standing project consent alone is insufficient: every root operation must
/// also match one of these broker-owned attachments for the active conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct RootAttachment {
    conversation_id: Uuid,
    root_id: RootId,
}

impl RootAttachment {
    pub(crate) fn new(conversation_id: Uuid, root_id: RootId) -> Result<Self, GrantError> {
        if conversation_id.is_nil() {
            return Err(GrantError::NilConversation);
        }
        Ok(Self {
            conversation_id,
            root_id,
        })
    }

    /// Conversation that explicitly selected this root.
    pub const fn conversation_id(self) -> Uuid {
        self.conversation_id
    }

    /// Attached broker root.
    pub const fn root_id(self) -> RootId {
        self.root_id
    }
}

impl<'de> Deserialize<'de> for RootAttachment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireAttachment {
            conversation_id: Uuid,
            root_id: RootId,
        }

        let wire = WireAttachment::deserialize(deserializer)?;
        Self::new(wire.conversation_id, wire.root_id).map_err(D::Error::custom)
    }
}

/// Invalid trusted-control or persisted grant data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GrantError {
    /// Capability and scope do not describe a meaningful operation class.
    #[error("capability cannot be granted with this scope")]
    InvalidCapabilityScope,
    /// A subtree grant must name at least one path segment.
    #[error("a subtree grant cannot cover the root")]
    EmptySubtree,
    /// Nil conversation IDs cannot own attachments.
    #[error("attachment conversation must not be nil")]
    NilConversation,
}

fn validate_capability_scope(capability: Capability, scope: &Scope) -> Result<(), GrantError> {
    match (capability, scope) {
        (Capability::ListRoots, Scope::Subject)
        | (
            Capability::ReadFiles | Capability::WriteFiles | Capability::ExecuteCommands,
            Scope::Root { .. },
        ) => Ok(()),
        // A command is handed a folder, not a subtree of one. Accepting a
        // subtree scope here would record a confinement nothing enforces.
        (Capability::ExecuteCommands, Scope::PathSubtree { .. }) => {
            Err(GrantError::InvalidCapabilityScope)
        }
        (Capability::ReadFiles | Capability::WriteFiles, Scope::PathSubtree { relative, .. })
            if !relative.is_root() =>
        {
            Ok(())
        }
        (Capability::ReadFiles | Capability::WriteFiles, Scope::PathSubtree { .. }) => {
            Err(GrantError::EmptySubtree)
        }
        // App-scoped computer use: reading content or driving the app. Capture
        // may also be app-scoped (every window of one app).
        (
            Capability::ReadAppContent | Capability::ControlApp | Capability::CaptureScreen,
            Scope::App { bundle_id },
        ) if !bundle_id.trim().is_empty() => Ok(()),
        // Whole-display capture is its own scope; reads and control are never
        // display-scoped.
        (Capability::CaptureScreen, Scope::Screen) => Ok(()),
        _ => Err(GrantError::InvalidCapabilityScope),
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resource<'a> {
    Subject,
    Root(&'a RootId),
    Path {
        root_id: &'a RootId,
        relative: &'a RelativePath,
    },
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Authorization {
    pub(crate) grant_id: GrantId,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Denied {
    pub(crate) capability: Capability,
}

#[cfg(test)]
pub(crate) fn authorize(
    grants: &[Grant],
    attachments: &[RootAttachment],
    context: ExecutionContext,
    capability: Capability,
    resource: Resource<'_>,
) -> Result<Authorization, Denied> {
    let attached = match resource {
        Resource::Subject => true,
        Resource::Root(root_id) | Resource::Path { root_id, .. } => {
            attachments.iter().any(|item| {
                item.conversation_id == context.conversation_id() && item.root_id == *root_id
            })
        }
    };
    if !attached {
        return Err(Denied { capability });
    }

    grants
        .iter()
        .find(|grant| {
            context.grant_subject_matches(grant.subject)
                && grant.capability == capability
                && scope_covers(&grant.scope, resource)
        })
        .map(|grant| Authorization { grant_id: grant.id })
        .ok_or(Denied { capability })
}

#[cfg(test)]
fn scope_covers(scope: &Scope, resource: Resource<'_>) -> bool {
    match (scope, resource) {
        (Scope::Subject, Resource::Subject) => true,
        (Scope::Root { root_id }, Resource::Root(requested)) => root_id == requested,
        (
            Scope::Root { root_id },
            Resource::Path {
                root_id: requested, ..
            },
        ) => root_id == requested,
        (
            Scope::PathSubtree { root_id, relative },
            Resource::Path {
                root_id: requested,
                relative: requested_relative,
            },
        ) => root_id == requested && path_starts_with(requested_relative, relative),
        _ => false,
    }
}

#[cfg(test)]
fn path_starts_with(candidate: &RelativePath, prefix: &RelativePath) -> bool {
    let candidate = candidate.segments().collect::<Vec<_>>();
    let prefix = prefix.segments().collect::<Vec<_>>();
    candidate.len() >= prefix.len()
        && prefix
            .iter()
            .zip(candidate.iter())
            .all(|(left, right)| left == right)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn subject_project(id: Uuid) -> GrantSubject {
        GrantSubject::project(id).unwrap()
    }

    fn subject_conversation(id: Uuid) -> GrantSubject {
        GrantSubject::conversation(id).unwrap()
    }

    fn grant(subject: GrantSubject, capability: Capability, scope: Scope) -> Grant {
        Grant::from_consent(
            GrantId::new(),
            subject,
            capability,
            scope,
            ConsentRecord::new(
                ConsentMethod::FolderPicker,
                Utc.with_ymd_and_hms(2026, 7, 14, 0, 0, 0).unwrap(),
            ),
        )
        .unwrap()
    }

    fn attachment(conversation_id: Uuid, root_id: RootId) -> RootAttachment {
        RootAttachment::new(conversation_id, root_id).unwrap()
    }

    #[test]
    fn denies_without_an_exact_live_grant_and_attachment() {
        let conversation = Uuid::new_v4();
        let context = ExecutionContext::standalone(conversation).unwrap();
        let root = RootId::new();
        let expected = Err(Denied {
            capability: Capability::ReadFiles,
        });
        assert_eq!(
            authorize(
                &[],
                &[attachment(conversation, root)],
                context,
                Capability::ReadFiles,
                Resource::Root(&root),
            ),
            expected
        );
        let grants = [grant(
            subject_conversation(conversation),
            Capability::ReadFiles,
            Scope::Root { root_id: root },
        )];
        assert_eq!(
            authorize(
                &grants,
                &[],
                context,
                Capability::ReadFiles,
                Resource::Root(&root),
            ),
            expected
        );
    }

    #[test]
    fn project_consent_is_intersected_with_conversation_attachment() {
        let project = Uuid::new_v4();
        let attached_conversation = Uuid::new_v4();
        let other_conversation = Uuid::new_v4();
        let root = RootId::new();
        let grants = [grant(
            subject_project(project),
            Capability::ReadFiles,
            Scope::Root { root_id: root },
        )];
        let attachments = [attachment(attached_conversation, root)];

        assert!(authorize(
            &grants,
            &attachments,
            ExecutionContext::project_chat(attached_conversation, project).unwrap(),
            Capability::ReadFiles,
            Resource::Root(&root),
        )
        .is_ok());
        assert!(authorize(
            &grants,
            &attachments,
            ExecutionContext::project_chat(other_conversation, project).unwrap(),
            Capability::ReadFiles,
            Resource::Root(&root),
        )
        .is_err());
    }

    #[test]
    fn subject_wide_root_discovery_does_not_require_a_root_attachment() {
        let project = Uuid::new_v4();
        let conversation = Uuid::new_v4();
        let grants = [grant(
            subject_project(project),
            Capability::ListRoots,
            Scope::Subject,
        )];
        assert!(authorize(
            &grants,
            &[],
            ExecutionContext::project_chat(conversation, project).unwrap(),
            Capability::ListRoots,
            Resource::Subject,
        )
        .is_ok());
    }

    #[test]
    fn rejects_a_grant_from_another_project_or_conversation() {
        let conversation = Uuid::new_v4();
        let project = Uuid::new_v4();
        let root = RootId::new();
        let attachments = [attachment(conversation, root)];
        let context = ExecutionContext::project_chat(conversation, project).unwrap();
        for subject in [
            subject_project(Uuid::new_v4()),
            subject_conversation(Uuid::new_v4()),
        ] {
            let grants = [grant(
                subject,
                Capability::ReadFiles,
                Scope::Root { root_id: root },
            )];
            assert!(authorize(
                &grants,
                &attachments,
                context,
                Capability::ReadFiles,
                Resource::Root(&root),
            )
            .is_err());
        }
    }

    #[test]
    fn selects_the_exact_root_in_a_multi_root_context() {
        let conversation = Uuid::new_v4();
        let context = ExecutionContext::standalone(conversation).unwrap();
        let allowed = RootId::new();
        let other = RootId::new();
        let grants = [grant(
            subject_conversation(conversation),
            Capability::ReadFiles,
            Scope::Root { root_id: allowed },
        )];
        let attachments = [
            attachment(conversation, allowed),
            attachment(conversation, other),
        ];
        assert!(authorize(
            &grants,
            &attachments,
            context,
            Capability::ReadFiles,
            Resource::Root(&allowed),
        )
        .is_ok());
        assert!(authorize(
            &grants,
            &attachments,
            context,
            Capability::ReadFiles,
            Resource::Root(&other),
        )
        .is_err());
    }

    #[test]
    fn subtree_grants_cover_only_segment_aligned_descendants() {
        let conversation = Uuid::new_v4();
        let context = ExecutionContext::standalone(conversation).unwrap();
        let root = RootId::new();
        let subtree = RelativePath::parse("reports/approved").unwrap();
        let inside = RelativePath::parse("reports/approved/q1.md").unwrap();
        let sibling = RelativePath::parse("reports/approved-old/q1.md").unwrap();
        let grants = [grant(
            subject_conversation(conversation),
            Capability::ReadFiles,
            Scope::PathSubtree {
                root_id: root,
                relative: subtree,
            },
        )];
        let attachments = [attachment(conversation, root)];
        assert!(authorize(
            &grants,
            &attachments,
            context,
            Capability::ReadFiles,
            Resource::Path {
                root_id: &root,
                relative: &inside,
            },
        )
        .is_ok());
        assert!(authorize(
            &grants,
            &attachments,
            context,
            Capability::ReadFiles,
            Resource::Path {
                root_id: &root,
                relative: &sibling,
            },
        )
        .is_err());
    }

    #[test]
    fn invalid_capability_scope_pairs_cannot_deserialize() {
        let subject = subject_project(Uuid::new_v4());
        let consent = ConsentRecord::new(ConsentMethod::FolderPicker, Utc::now());
        let invalid = serde_json::json!({
            "id": GrantId::new(),
            "subject": subject,
            "capability": "list_roots",
            "scope": { "kind": "root", "root_id": RootId::new() },
            "consent": consent,
        });
        assert!(serde_json::from_value::<Grant>(invalid).is_err());
    }

    #[test]
    fn revocation_is_a_fresh_snapshot_not_a_cached_directory_handle() {
        let conversation = Uuid::new_v4();
        let context = ExecutionContext::standalone(conversation).unwrap();
        let root = RootId::new();
        let mut grants = vec![grant(
            subject_conversation(conversation),
            Capability::ReadFiles,
            Scope::Root { root_id: root },
        )];
        let attachments = [attachment(conversation, root)];
        assert!(authorize(
            &grants,
            &attachments,
            context,
            Capability::ReadFiles,
            Resource::Root(&root),
        )
        .is_ok());
        grants.clear();
        assert!(authorize(
            &grants,
            &attachments,
            context,
            Capability::ReadFiles,
            Resource::Root(&root),
        )
        .is_err());
    }

    #[test]
    fn control_app_implies_both_reads() {
        use Capability::{CaptureScreen, ControlApp, ReadAppContent};
        assert!(Capability::implies(ControlApp, ReadAppContent));
        assert!(Capability::implies(ControlApp, CaptureScreen));
    }

    #[test]
    fn the_two_read_capabilities_imply_each_other() {
        use Capability::{CaptureScreen, ReadAppContent};
        assert!(Capability::implies(ReadAppContent, CaptureScreen));
        assert!(Capability::implies(CaptureScreen, ReadAppContent));
    }

    #[test]
    fn implication_is_one_directional_from_control_to_reads() {
        use Capability::{CaptureScreen, ControlApp, ReadAppContent};
        // A read grant must never confer control.
        assert!(!Capability::implies(ReadAppContent, ControlApp));
        assert!(!Capability::implies(CaptureScreen, ControlApp));
    }

    #[test]
    fn folder_capabilities_imply_only_themselves() {
        use Capability::{CaptureScreen, ControlApp, ExecuteCommands, ReadFiles};
        // Folder exec does not reach computer use, and vice versa.
        assert!(!Capability::implies(ExecuteCommands, ControlApp));
        assert!(!Capability::implies(ControlApp, ExecuteCommands));
        assert!(!Capability::implies(ReadFiles, CaptureScreen));
        // Reflexivity holds for every capability.
        assert!(Capability::implies(ExecuteCommands, ExecuteCommands));
        assert!(Capability::implies(ControlApp, ControlApp));
    }

    #[test]
    fn app_and_screen_scopes_validate_against_their_capabilities() {
        let subject = subject_project(Uuid::new_v4());
        let consent = || {
            ConsentRecord::new(
                ConsentMethod::FolderPicker,
                Utc.with_ymd_and_hms(2026, 7, 14, 0, 0, 0).unwrap(),
            )
        };
        let app = || Scope::App {
            bundle_id: "com.apple.Notes".to_string(),
        };

        // Valid pairings.
        for (capability, scope) in [
            (Capability::ReadAppContent, app()),
            (Capability::ControlApp, app()),
            (Capability::CaptureScreen, app()),
            (Capability::CaptureScreen, Scope::Screen),
        ] {
            let scope_debug = format!("{scope:?}");
            Grant::from_consent(GrantId::new(), subject, capability, scope, consent())
                .unwrap_or_else(|_| panic!("{capability:?} with {scope_debug} should be valid"));
        }

        // Reads and control are never display-scoped; capture is never
        // subject-scoped; an empty bundle id is rejected.
        for (capability, scope) in [
            (Capability::ReadAppContent, Scope::Screen),
            (Capability::ControlApp, Scope::Screen),
            (Capability::CaptureScreen, Scope::Subject),
            (
                Capability::ControlApp,
                Scope::App {
                    bundle_id: "   ".to_string(),
                },
            ),
        ] {
            assert!(
                Grant::from_consent(GrantId::new(), subject, capability, scope, consent()).is_err()
            );
        }
    }
}
