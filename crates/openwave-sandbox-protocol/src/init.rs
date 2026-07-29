//! The run init the host delivers after the sandbox handle commits.
//!
//! Only after the handle is committed onto the run row does the host deliver
//! the task, the scoped token, and the policy snapshot — a sandbox reclaimed
//! before that point never executed anything. Init is delivered once per
//! attempt; a sandbox-resident run has exactly one execution attempt.
//!
//! No long-lived credential ever enters this payload. A detached-admitted run
//! carries a short-lived, scoped token; an attached-only run proxies inference
//! through the host and carries none.

use serde::{Deserialize, Serialize};

use crate::{
    ids::RunId,
    reverse::{Capability, RunProvenance},
};

/// A short-lived, scoped model credential, held by the supervisor and never the
/// agent. Absent for an attached-only run, which proxies inference through the
/// host over the reverse channel.
///
/// Never logged or `Debug`-printed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopedModelToken(String);

impl ScopedModelToken {
    /// Wrap a minted token.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The raw token, for the supervisor's egress boundary.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ScopedModelToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ScopedModelToken([redacted])")
    }
}

/// The policy snapshot the run enforces while unattached.
///
/// For an unattached run, policy is this snapshot delivered at admission;
/// revoking a grant while the run is unattached takes effect at reattachment,
/// not instantly. On a supervisor-only enforcement tier these entries are
/// policy, not a boundary — a fact the host tracks out-of-band, never a wire
/// claim the backend makes about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySnapshot {
    /// Destinations the run may open a connection to, snapshotted at admission.
    pub egress_allowlist: Vec<String>,
    /// The reverse-RPC capabilities granted to this run, deny-by-default.
    pub granted_capabilities: Vec<Capability>,
}

/// Whether the run may keep working while unattached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionMode {
    /// The default: must not work while unattached; checkpoints and waits.
    AttachedOnly,
    /// May keep working through host absence, within the run's bounds.
    Detached,
}

/// The task and context the host delivers to a freshly provisioned sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunInit {
    /// The run this init belongs to.
    pub run_id: RunId,
    /// Provenance for audit and consent-prompt rendering.
    pub provenance: RunProvenance,
    /// The delegated task, as bounded UTF-8.
    pub task: String,
    /// Absolute deadline for the whole run, in Unix seconds.
    pub deadline_unix_secs: u64,
    /// Whether the run may work unattached.
    pub admission: AdmissionMode,
    /// The policy the supervisor enforces while unattached.
    pub policy: PolicySnapshot,
    /// The scoped model token for a detached run; absent for attached-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_token: Option<ScopedModelToken>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_roundtrips_and_omits_absent_token() {
        let init = RunInit {
            run_id: RunId::new(),
            provenance: RunProvenance {
                run_id: RunId::new(),
                provider: "reference".to_owned(),
            },
            task: "summarize the corpus".to_owned(),
            deadline_unix_secs: 1_800_000_000,
            admission: AdmissionMode::AttachedOnly,
            policy: PolicySnapshot {
                egress_allowlist: vec![],
                granted_capabilities: vec![Capability::ModelInference],
            },
            scoped_token: None,
        };
        let encoded = serde_json::to_value(&init).unwrap();
        assert!(encoded.get("scoped_token").is_none());
        assert_eq!(serde_json::from_value::<RunInit>(encoded).unwrap(), init);
    }

    #[test]
    fn scoped_token_and_transport_secret_do_not_leak_in_debug() {
        let token = ScopedModelToken::new("super-secret");
        assert!(!format!("{token:?}").contains("super-secret"));
    }
}
