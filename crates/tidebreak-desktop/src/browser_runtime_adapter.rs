//! Desktop implementation of the server-owned browser runtime boundary.
//!
//! The HTTP channel authenticates a code session and passes its exact
//! `{owner, workspace, session}` scope here. This adapter maps that scope onto
//! a short-lived native [`BrowserRegistry`] capability. Revocation leaves an
//! enduring tombstone: an ended session id can never lazily mint a replacement
//! capability on a later request.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use tauri::AppHandle;
use tidebreak_core::{
    BrowserListResult, BrowserNavigateArgs, BrowserNavigateResult, BrowserPageSnapshot,
    BrowserSnapshotArgs, CodeSessionId, OwnerId, WorkspaceId,
};
use tidebreak_server::{BrowserRuntime, BrowserRuntimeError, BrowserRuntimeScope};
use uuid::Uuid;

use crate::browser_control::BrowserRegistry;

#[derive(Clone)]
pub(crate) struct DesktopBrowserRuntime {
    app: AppHandle,
    registry: BrowserRegistry,
    sessions: SessionCapabilities,
}

impl DesktopBrowserRuntime {
    pub(crate) fn new(app: AppHandle, registry: BrowserRegistry) -> Self {
        Self {
            app,
            registry,
            sessions: SessionCapabilities::default(),
        }
    }
}

#[async_trait]
impl BrowserRuntime for DesktopBrowserRuntime {
    async fn list(
        &self,
        scope: &BrowserRuntimeScope,
    ) -> Result<BrowserListResult, BrowserRuntimeError> {
        let capability_id = self.sessions.capability_for(&self.registry, scope)?;
        self.registry
            .list_for_capability(capability_id)
            .map(|sessions| BrowserListResult { sessions })
            .map_err(|error| map_native_error(None, error))
    }

    async fn navigate(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserNavigateArgs,
    ) -> Result<BrowserNavigateResult, BrowserRuntimeError> {
        let capability_id = self.sessions.capability_for(&self.registry, scope)?;
        crate::code_browser::navigate_browser_for_agent(
            &self.app,
            &self.registry,
            capability_id,
            args,
        )
        .await
        .map_err(|error| map_native_error(Some(&args.browser_id), error))
    }

    async fn snapshot(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserSnapshotArgs,
    ) -> Result<BrowserPageSnapshot, BrowserRuntimeError> {
        let capability_id = self.sessions.capability_for(&self.registry, scope)?;
        crate::browser_semantics::browser_semantic_snapshot(
            &self.app,
            &self.registry,
            capability_id,
            args.clone(),
        )
        .await
        .map_err(|error| map_native_error(Some(&args.browser_id), error))
    }

    fn revoke_session(&self, scope: &BrowserRuntimeScope) {
        self.sessions.revoke(&self.registry, scope);
    }
}

#[derive(Clone, Default)]
struct SessionCapabilities {
    states: std::sync::Arc<Mutex<HashMap<CodeSessionId, SessionCapabilityState>>>,
}

enum SessionCapabilityState {
    Active(ActiveSessionCapability),
    Revoked,
}

struct ActiveSessionCapability {
    owner: OwnerId,
    workspace: WorkspaceId,
    capability_id: Uuid,
}

impl SessionCapabilities {
    fn capability_for(
        &self,
        registry: &BrowserRegistry,
        scope: &BrowserRuntimeScope,
    ) -> Result<Uuid, BrowserRuntimeError> {
        use std::collections::hash_map::Entry;

        let mut states = self.lock();
        match states.entry(scope.session) {
            Entry::Occupied(mut entry) => {
                if matches!(entry.get(), SessionCapabilityState::Revoked) {
                    return Err(BrowserRuntimeError::SessionEnded);
                }

                let scope_matches = match entry.get() {
                    SessionCapabilityState::Active(active) => {
                        active.owner == scope.owner && active.workspace == scope.workspace
                    }
                    SessionCapabilityState::Revoked => false,
                };
                if !scope_matches {
                    let previous =
                        match std::mem::replace(entry.get_mut(), SessionCapabilityState::Revoked) {
                            SessionCapabilityState::Active(active) => active.capability_id,
                            SessionCapabilityState::Revoked => unreachable!("checked above"),
                        };
                    registry.revoke_agent_capability(previous);
                    return Err(BrowserRuntimeError::SessionEnded);
                }

                let previous = match entry.get() {
                    SessionCapabilityState::Active(active) => active.capability_id,
                    SessionCapabilityState::Revoked => unreachable!("checked above"),
                };
                let workspace = scope.workspace.to_string();
                if registry
                    .heartbeat_agent_capability(previous, &workspace)
                    .is_ok()
                {
                    return Ok(previous);
                }

                // A live code session may outlast the native capability's
                // short TTL. Rotate it atomically under both registries so a
                // concurrent revoke cannot resurrect the session and active
                // controller ownership does not become a permanent Stop.
                match registry.rotate_expired_agent_capability(previous, &workspace, "Code agent") {
                    Ok(replacement) => {
                        match entry.get_mut() {
                            SessionCapabilityState::Active(active) => {
                                active.capability_id = replacement;
                            }
                            SessionCapabilityState::Revoked => unreachable!("checked above"),
                        }
                        Ok(replacement)
                    }
                    Err(_) => {
                        *entry.get_mut() = SessionCapabilityState::Revoked;
                        registry.revoke_agent_capability(previous);
                        Err(BrowserRuntimeError::SessionEnded)
                    }
                }
            }
            Entry::Vacant(entry) => {
                let capability_id =
                    registry.issue_agent_capability(&scope.workspace.to_string(), "Code agent");
                entry.insert(SessionCapabilityState::Active(ActiveSessionCapability {
                    owner: scope.owner.clone(),
                    workspace: scope.workspace,
                    capability_id,
                }));
                Ok(capability_id)
            }
        }
    }

    fn revoke(&self, registry: &BrowserRegistry, scope: &BrowserRuntimeScope) {
        use std::collections::hash_map::Entry;

        let capability_id = {
            let mut states = self.lock();
            match states.entry(scope.session) {
                Entry::Occupied(mut entry) => {
                    match std::mem::replace(entry.get_mut(), SessionCapabilityState::Revoked) {
                        SessionCapabilityState::Active(active) => Some(active.capability_id),
                        SessionCapabilityState::Revoked => None,
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(SessionCapabilityState::Revoked);
                    None
                }
            }
        };
        if let Some(capability_id) = capability_id {
            registry.revoke_agent_capability(capability_id);
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<CodeSessionId, SessionCapabilityState>> {
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn map_native_error(browser_id: Option<&str>, error: String) -> BrowserRuntimeError {
    match error.as_str() {
        "browser capability is unavailable" => return BrowserRuntimeError::SessionEnded,
        "browser origin is not shared with this agent"
        | "browser origin is not shared for this operation"
        | "browser origin is not shared for control"
        | "browser control was stopped by the user"
        | "browser has no authorized HTTP origin" => {
            return BrowserRuntimeError::NotAuthorized(error);
        }
        "browser page is still loading"
        | "browser origin changed before dispatch"
        | "browser document changed while it was being inspected"
        | "browser document changed since the snapshot was taken"
        | "browser document changed while screenshot was being captured"
        | "browser snapshot is stale; take a new browser snapshot"
        | "browser session changed while control was transferring"
        | "browser session was replaced while waiting" => {
            return BrowserRuntimeError::StaleTarget;
        }
        "semantic browser control is not available on this platform yet" => {
            return BrowserRuntimeError::Unsupported("semantic snapshots".to_owned());
        }
        _ => {}
    }

    let Some(browser_id) = browser_id else {
        return BrowserRuntimeError::Failed(error);
    };
    if matches!(
        error.as_str(),
        "browser session is not registered"
            | "browser session is not open"
            | "browser is hidden"
            | "browser is not controlled by this agent"
            | "browser is controlled by another agent"
    ) || error == format!("browser session {browser_id} belongs to a different workspace")
    {
        return BrowserRuntimeError::UnknownBrowserId(browser_id.to_owned());
    }
    BrowserRuntimeError::Failed(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(session: CodeSessionId, workspace: WorkspaceId) -> BrowserRuntimeScope {
        BrowserRuntimeScope {
            owner: OwnerId::local(),
            workspace,
            session,
        }
    }

    #[test]
    fn revoke_before_first_use_is_an_enduring_tombstone() {
        let registry = BrowserRegistry::default();
        let sessions = SessionCapabilities::default();
        let scope = scope(CodeSessionId::new(), WorkspaceId::new());

        sessions.revoke(&registry, &scope);

        assert_eq!(
            sessions.capability_for(&registry, &scope),
            Err(BrowserRuntimeError::SessionEnded)
        );
    }

    #[test]
    fn revoked_session_never_reissues_a_native_capability() {
        let registry = BrowserRegistry::default();
        let sessions = SessionCapabilities::default();
        let scope = scope(CodeSessionId::new(), WorkspaceId::new());
        let capability_id = sessions.capability_for(&registry, &scope).unwrap();

        sessions.revoke(&registry, &scope);

        assert!(registry
            .heartbeat_agent_capability(capability_id, &scope.workspace.to_string())
            .is_err());
        assert_eq!(
            sessions.capability_for(&registry, &scope),
            Err(BrowserRuntimeError::SessionEnded)
        );
    }

    #[test]
    fn a_live_session_reuses_its_native_capability_across_channel_reissue() {
        let registry = BrowserRegistry::default();
        let sessions = SessionCapabilities::default();
        let scope = scope(CodeSessionId::new(), WorkspaceId::new());
        let workspace = scope.workspace.to_string();

        let first = sessions.capability_for(&registry, &scope).unwrap();
        let second = sessions.capability_for(&registry, &scope).unwrap();

        assert_eq!(second, first);
        assert!(registry
            .heartbeat_agent_capability(second, &workspace)
            .is_ok());
    }

    #[test]
    fn reused_session_id_with_another_scope_fails_closed() {
        let registry = BrowserRegistry::default();
        let sessions = SessionCapabilities::default();
        let session = CodeSessionId::new();
        let original = scope(session, WorkspaceId::new());
        let capability_id = sessions.capability_for(&registry, &original).unwrap();
        let replacement_scope = scope(session, WorkspaceId::new());

        assert_eq!(
            sessions.capability_for(&registry, &replacement_scope),
            Err(BrowserRuntimeError::SessionEnded)
        );
        assert!(registry
            .heartbeat_agent_capability(capability_id, &original.workspace.to_string())
            .is_err());
        assert_eq!(
            sessions.capability_for(&registry, &original),
            Err(BrowserRuntimeError::SessionEnded)
        );
    }

    #[test]
    fn expired_capability_rotates_without_stranding_agent_control() {
        use tidebreak_core::{BrowserGrantCapability, BrowserOrigin, BrowserOriginScope};

        let registry = BrowserRegistry::default();
        let sessions = SessionCapabilities::default();
        let scope = scope(CodeSessionId::new(), WorkspaceId::new());
        let workspace = scope.workspace.to_string();
        registry
            .register(
                "browser-1",
                &workspace,
                "https://example.com".to_owned(),
                true,
            )
            .unwrap();
        let origin = BrowserOrigin::from_url("https://example.com").unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                &workspace,
                &origin,
                BrowserOriginScope::Origin {
                    origin: origin.clone(),
                },
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap();

        let previous = sessions.capability_for(&registry, &scope).unwrap();
        registry.begin_agent_control(previous, "browser-1").unwrap();
        registry.expire_agent_capability_for_test(previous);

        let replacement = sessions.capability_for(&registry, &scope).unwrap();

        assert_ne!(replacement, previous);
        assert!(registry
            .heartbeat_agent_capability(previous, &workspace)
            .is_err());
        registry
            .begin_agent_control(replacement, "browser-1")
            .unwrap();
    }

    #[test]
    fn native_error_contract_maps_exact_status_categories() {
        let browser_id = "browser-1";
        let cases = [
            (
                "browser capability is unavailable",
                BrowserRuntimeError::SessionEnded,
            ),
            (
                "browser session is not registered",
                BrowserRuntimeError::UnknownBrowserId(browser_id.to_owned()),
            ),
            (
                "browser session is not open",
                BrowserRuntimeError::UnknownBrowserId(browser_id.to_owned()),
            ),
            (
                "browser is controlled by another agent",
                BrowserRuntimeError::UnknownBrowserId(browser_id.to_owned()),
            ),
            (
                "browser is hidden",
                BrowserRuntimeError::UnknownBrowserId(browser_id.to_owned()),
            ),
            (
                "browser origin is not shared with this agent",
                BrowserRuntimeError::NotAuthorized(
                    "browser origin is not shared with this agent".to_owned(),
                ),
            ),
            (
                "browser origin is not shared for this operation",
                BrowserRuntimeError::NotAuthorized(
                    "browser origin is not shared for this operation".to_owned(),
                ),
            ),
            (
                "browser page is still loading",
                BrowserRuntimeError::StaleTarget,
            ),
            (
                "browser control was stopped by the user",
                BrowserRuntimeError::NotAuthorized(
                    "browser control was stopped by the user".to_owned(),
                ),
            ),
            (
                "browser document changed while it was being inspected",
                BrowserRuntimeError::StaleTarget,
            ),
            (
                "semantic browser control is not available on this platform yet",
                BrowserRuntimeError::Unsupported("semantic snapshots".to_owned()),
            ),
        ];

        for (native, expected) in cases {
            assert_eq!(
                map_native_error(Some(browser_id), native.to_owned()),
                expected,
                "native error {native:?}"
            );
        }
        assert_eq!(
            map_native_error(
                Some(browser_id),
                format!("browser session {browser_id} belongs to a different workspace"),
            ),
            BrowserRuntimeError::UnknownBrowserId(browser_id.to_owned())
        );
    }

    #[test]
    fn native_error_contract_never_classifies_by_substring() {
        let message = "wrapper: browser page is still loading".to_owned();
        assert_eq!(
            map_native_error(Some("browser-1"), message.clone()),
            BrowserRuntimeError::Failed(message)
        );
    }
}
