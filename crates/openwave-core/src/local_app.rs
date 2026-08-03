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
use crate::id::{AgentRunId, AppId, AppRevisionId, ChatId, ConnectedAppId, TurnId};

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

/// The durable consent object for one app: the granted `(app, tools[])` set,
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

/// One granted binding: the tools the user consented to under one connected
/// app, pinned to the definition that record carried at consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppGrantBinding {
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
    validate_binding_set(
        grant
            .bindings
            .iter()
            .map(|binding| (binding.app, binding.tools.as_slice())),
    )?;
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

/// The mounted tools one connected app contributes to a local app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppBinding {
    /// Id of the connected app the tools belong to. The available ids are
    /// listed in the `create_app` tool description.
    #[schemars(description = "Id of the connected app these tools belong to.")]
    pub app: ConnectedAppId,
    /// Full mounted tool names, each `mcp__{namespace}__{tool}` under the
    /// bound connected app's namespace.
    #[schemars(
        description = "Full mounted tool names (`mcp__{namespace}__{tool}`), all \
                       under the bound connected app's namespace."
    )]
    pub tools: Vec<String>,
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
    validate_binding_set(
        manifest
            .bindings
            .iter()
            .map(|binding| (binding.app, binding.tools.as_slice())),
    )?;
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

/// The binding grammar shared by manifests and grants: no duplicate connected
/// apps, and every tool shaped like a full mounted name.
///
/// A namespace may itself contain `_`, so a mounted name cannot be split
/// unambiguously without knowing the namespace — and the bound record lives
/// behind the store. The grammar here is therefore structural only; the exact
/// `mcp__{namespace}__` cross-check against the bound connected app's
/// configuration belongs to the host layers that resolve ids (the
/// `create_app` door, grant computation, the invoke gate).
fn validate_binding_set<'a>(
    bindings: impl Iterator<Item = (ConnectedAppId, &'a [String])>,
) -> Result<(), String> {
    let mut apps = std::collections::HashSet::new();
    for (app, binding_tools) in bindings {
        if !apps.insert(app) {
            return Err(format!("duplicate binding for connected app {app}"));
        }
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
            bindings: vec![AppBinding {
                app,
                tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
            }],
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
                    AppBinding {
                        app,
                        tools: Vec::new()
                    },
                    AppBinding {
                        app,
                        tools: Vec::new()
                    },
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
