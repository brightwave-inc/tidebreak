//! Startup recovery: reconcile stored sessions and workspaces with the host.

use super::*;

impl CodeRuntime {
    /// Whether startup recovery must resolve the managed install before it
    /// probes `kind`.
    ///
    /// A headless embedding with no data directory, an embedding-declared
    /// binary, and the in-process engine have no managed install to prepare.
    /// Debug end-to-end tests can also replace every adapter with the scripted
    /// engine; preserve that declared test surface instead of downloading a
    /// real package behind it.
    fn recovery_uses_managed_harness(&self, kind: HarnessKind) -> bool {
        #[cfg(debug_assertions)]
        if crate::scripted_harness::env_is_set() {
            return false;
        }
        !kind.is_in_process()
            && self.host.data_dir.is_some()
            && self.host.declared(kind).is_none()
            && tidebreak_harness::pin_for(kind).is_some()
    }

    /// Install and probe each managed engine needed by recovered sessions.
    ///
    /// Pin bumps leave the previous version on disk. A session-create request
    /// resolves the selected pin before probing, but startup recovery used to
    /// probe first and strand every saved session on `harness_not_found`.
    /// Group by kind so many saved sessions share one install and one cold
    /// probe. Recovery already runs after bind, so these downloads do not hold
    /// the server port closed.
    async fn prepare_recovered_harnesses(&self, sessions: &[CodeSession]) -> HashSet<HarnessKind> {
        let mut kinds = sessions
            .iter()
            .map(|session| session.harness_kind)
            .filter(|kind| self.recovery_uses_managed_harness(*kind))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        kinds.sort_by_key(|kind| kind.as_str());
        let prepared = futures::future::join_all(kinds.into_iter().map(|kind| async move {
            let result = self.ensure_harness(kind, false, false).await;
            (kind, result)
        }))
        .await;
        let mut unavailable = HashSet::new();
        for (kind, result) in prepared {
            match result {
                Ok(installed) => {
                    self.record_pin_install(kind, Ok(()));
                    self.invalidate_moved_probe(kind, &installed.binary);
                    if let Ok(adapter) = self.adapter(kind) {
                        self.probe(adapter.as_ref()).await;
                    }
                }
                Err(error) => {
                    self.record_pin_install(kind, Err(error.clone()));
                    let session_count = sessions
                        .iter()
                        .filter(|session| session.harness_kind == kind)
                        .count();
                    tracing::warn!(
                        %kind,
                        sessions = session_count,
                        %error,
                        "code-mode: could not prepare the engine for recovered session workers"
                    );
                    unavailable.insert(kind);
                }
            }
        }
        unavailable
    }

    /// Revoke the session browser token and permanently tombstone its native
    /// adapter authority. Idempotent — safe to call multiple times.
    ///
    /// Terminal paths pass the database-backed session scope explicitly so
    /// the adapter is still tombstoned when a prior launch failure already
    /// removed the transient bearer token and capfile.
    pub(super) fn revoke_browser_session(&self, session: &CodeSession) {
        self.browser_tokens.revoke(session.id);
        self.approvals.revoke_session(session.id);
        if let Some(relay) = &self.harness_llm {
            relay.revoke(session.id);
        }
        if let (Some(runtime), Some(workspace)) = (&self.browser_runtime, session.workspace_id) {
            let scope = crate::code::browser_runtime::BrowserRuntimeScope {
                owner: session.owner.clone(),
                workspace,
                session: session.id,
            };
            runtime.revoke_session(&scope);
        }
    }

    /// Revoke only the outgoing worker's transient browser and approval channels.
    ///
    /// Reap and launch-failure paths replace a worker while preserving the
    /// same logical code session. Its native browser capability therefore
    /// stays live and can be reused by the fresh channel. Only terminal end
    /// paths call [`Self::revoke_browser_session`] and plant the adapter's
    /// enduring tombstone.
    pub(super) fn revoke_worker_channels(&self, session_id: CodeSessionId) {
        self.browser_tokens.revoke(session_id);
        self.approvals.revoke_session(session_id);
        if let Some(relay) = &self.harness_llm {
            relay.revoke(session_id);
        }
    }

    pub(crate) async fn recover(&self) -> Result<Vec<RecoveryAction>, ServerError> {
        self.ensure_stall_sweep();
        let mut actions = Vec::new();
        let mut recovery_owners = list_sessions_all_owners(&self.db)
            .await?
            .into_iter()
            .map(|session| session.owner)
            .collect::<Vec<_>>();
        recovery_owners.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        recovery_owners.dedup();
        for owner in &recovery_owners {
            for pending in list_pending_permission_mode_changes(&self.db, owner).await? {
                let reason = FenceReason::ProbeAmbiguous {
                    detail: format!(
                        "permission mode change from {} to {} stopped before revision {} committed",
                        pending.intent.previous_mode,
                        pending.intent.requested_mode,
                        pending.intent.revision
                    ),
                };
                if let Some(fenced) =
                    fence_permission_mode_change(&self.db, owner, &pending.intent, &reason).await?
                {
                    crate::code::attention::emit_digest(&self.db, &self.bus, &fenced).await;
                    actions.push(RecoveryAction::Fenced {
                        session: fenced.id.to_string(),
                    });
                } else {
                    let _ =
                        discard_permission_mode_change(&self.db, owner, &pending.intent).await?;
                }
            }
        }
        actions.extend(
            recovery::recover_running_sessions(&self.db, &self.bus)
                .await
                .map_err(ServerError::from)?,
        );
        for workspace in
            list_workspaces_by_status_all_owners(&self.db, CodeWorkspaceStatus::Archiving).await?
        {
            let sessions =
                list_sessions_for_workspace(&self.db, &workspace.owner, workspace.id).await?;
            if sessions.iter().any(|session| {
                matches!(
                    session.lifecycle,
                    CodeSessionLifecycle::Running | CodeSessionLifecycle::Fenced
                )
            }) {
                tracing::warn!(
                    workspace = %workspace.id,
                    "code-mode: archive recovery kept lifecycle exclusion because a worker may still exist"
                );
                continue;
            }
            if !std::path::Path::new(&workspace.worktree_path).exists() {
                let repo = self.get_repo(&workspace.owner, workspace.repo_id).await?;
                match self.finalize_removed_workspace(workspace, &repo).await {
                    Ok(workspace) => self.forget_workspace_turn_lock(workspace.id),
                    Err(error) => {
                        tracing::warn!(
                            "code-mode: archive recovery could not finalize a missing checkout: {}",
                            error.message()
                        );
                    }
                }
                continue;
            }
            if !compare_and_set_workspace_status(
                &self.db,
                &workspace.owner,
                workspace.id,
                CodeWorkspaceStatus::Archiving,
                CodeWorkspaceStatus::Active,
            )
            .await?
            {
                tracing::warn!(
                    workspace = %workspace.id,
                    "code-mode: archive recovery lost its lifecycle compare-and-set"
                );
            }
        }
        let recovered_sessions = list_sessions_all_owners(&self.db).await?;
        for session in &recovered_sessions {
            crate::code::approval_sweep::abandon_for_restart(
                &self.db,
                &self.bus,
                &session.owner,
                session.id,
                session.spawn_epoch,
            )
            .await;
            self.refresh_approval_attention(&session.owner, session.id)
                .await;
        }
        // Do not sweep repository-wide checkpoint refs here. Another
        // Tidebreak profile can manage the same repository from a separate
        // database, so this process cannot identify global orphans safely.
        // Recovery only mutates rows. Re-attach a worker for every session
        // that is still usable so submit_turn is not stuck after a restart.
        // Concurrently: each attach launches an engine child, so a serial pass
        // charged app launch the sum of every restored session.
        // Remote sessions have no local worker to re-attach: their engine
        // lives in a sandbox, and spawning a host harness against the empty
        // worktree path would fail loudly for a session that is healthy.
        let mut remote_workspaces = std::collections::HashSet::new();
        let mut checked_workspaces = std::collections::HashSet::new();
        for session in &recovered_sessions {
            if !checked_workspaces.insert(session.workspace_id) {
                continue;
            }
            if let Ok(Some(workspace)) = self.session_workspace(session).await {
                if workspace.is_remote() {
                    remote_workspaces.insert(session.workspace_id);
                }
            }
        }
        let resumable: Vec<CodeSession> = recovered_sessions
            .into_iter()
            .filter(|session| {
                !matches!(
                    session.lifecycle,
                    CodeSessionLifecycle::Ended | CodeSessionLifecycle::Fenced
                ) && !remote_workspaces.contains(&session.workspace_id)
                    && !self
                        .workers
                        .lock()
                        .expect("code workers")
                        .contains_key(&session.id)
            })
            .collect();
        let unavailable = self.prepare_recovered_harnesses(&resumable).await;
        futures::future::join_all(resumable.into_iter().filter_map(|session| {
            (!unavailable.contains(&session.harness_kind)).then_some(async move {
                // An install can take minutes. Re-read the row and the worker
                // map so a user ending or reaping the session during that wait
                // does not get an obsolete second worker afterward.
                let latest = match get_session(&self.db, &session.owner, session.id).await {
                    Ok(Some(latest))
                        if !matches!(
                            latest.lifecycle,
                            CodeSessionLifecycle::Ended | CodeSessionLifecycle::Fenced
                        ) && !self
                            .workers
                            .lock()
                            .expect("code workers")
                            .contains_key(&latest.id) =>
                    {
                        latest
                    }
                    Ok(_) => return,
                    Err(error) => {
                        tracing::warn!(
                            session = %session.id,
                            %error,
                            "code-mode: could not re-read a recovered session before attaching its worker"
                        );
                        return;
                    }
                };
                if let Err(error) = self.attach_and_spawn_worker(latest).await {
                    tracing::warn!(
                        "code-mode: could not resume a recovered session worker: {}",
                        error.message()
                    );
                }
            })
        }))
        .await;
        Ok(actions)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use tidebreak_code_execution::{HostDep, HostToolBroker, HostToolStatus};
    use tidebreak_core::HarnessCaps;
    use tidebreak_harness::HarnessSession;

    use super::*;
    use crate::code::harness_release::test_support::write_install;
    use crate::scripted_harness::{plain_text_script, ScriptedAdapter};

    struct AvailableNodeBroker {
        ensure_calls: AtomicUsize,
        root: PathBuf,
    }

    #[async_trait]
    impl HostToolBroker for AvailableNodeBroker {
        fn ensure(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
            self.ensure_calls.fetch_add(1, Ordering::Relaxed);
        }

        fn retry(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
        }

        async fn status(&self, tool: HostDep) -> HostToolStatus {
            assert_eq!(tool, HostDep::Node);
            HostToolStatus::Available
        }

        async fn managed_root(&self, tool: HostDep) -> Option<PathBuf> {
            assert_eq!(tool, HostDep::Node);
            Some(self.root.clone())
        }
    }

    /// The scripted engine reports missing until recovery has installed the
    /// selected managed pin. This makes worker attachment prove the ordering,
    /// without launching the real external engine in a unit test.
    struct PinCheckingAdapter {
        inner: ScriptedAdapter,
        expected_pin: PathBuf,
    }

    #[async_trait]
    impl HarnessAdapter for PinCheckingAdapter {
        fn kind(&self) -> HarnessKind {
            self.inner.kind()
        }

        async fn probe(&self, host: &HostEnv) -> HarnessProbe {
            if !self.expected_pin.is_file() {
                return HarnessProbe {
                    found: false,
                    binary_path: None,
                    version: None,
                    authenticated: None,
                    stderr: "the selected pin is missing".into(),
                    env: Vec::new(),
                    commands: Vec::new(),
                };
            }
            self.inner.probe(host).await
        }

        fn capabilities(&self, probe: &HarnessProbe) -> HarnessCaps {
            self.inner.capabilities(probe)
        }

        async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
            assert!(
                self.expected_pin.is_file(),
                "recovery must install the selected pin before launch"
            );
            self.inner.launch(spec).await
        }
    }

    fn write_executable(path: &Path, contents: &[u8]) {
        std::fs::create_dir_all(path.parent().expect("executable parent")).unwrap();
        std::fs::write(path, contents).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn saved_session(owner: &OwnerId, kind: HarnessKind) -> CodeSession {
        CodeSession {
            id: CodeSessionId::new(),
            owner: owner.clone(),
            workspace_id: None,
            kind: CodeSessionKind::Interactive,
            harness_kind: kind,
            harness_version: Some("0.147.0".into()),
            harness_resume_ref: Some("saved-session".into()),
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::new(
                tidebreak_core::AttentionState::Idle,
                AttentionSource::Lifecycle,
            ),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn recovery_installs_one_selected_pin_before_attaching_same_kind_sessions() {
        let data_dir = tempfile::tempdir().unwrap();
        let node_root = data_dir.path().join("fake-node");
        write_executable(
            &tidebreak_managed_node::managed_node_executable(&node_root),
            b"#!/bin/sh\nexit 0\n",
        );
        write_executable(
            &tidebreak_managed_node::managed_npm_executable(&node_root),
            b"#!/bin/sh\nset -eu\nmkdir -p node_modules/.bin\nprintf '#!/bin/sh\\nexit 0\\n' > node_modules/.bin/codex\nchmod +x node_modules/.bin/codex\n",
        );

        let kind = HarnessKind::Codex;
        let pin = tidebreak_harness::pin_for(kind).unwrap();
        let old_binary = write_install(data_dir.path(), kind, "0.147.0");
        let expected_pin = tidebreak_harness::pin::install_dir(data_dir.path(), pin)
            .join("node_modules")
            .join(".bin")
            .join(pin.bin);
        assert!(old_binary.is_file());
        assert!(!expected_pin.exists());

        let db = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                data_dir.path().join("code.db").display()
            ))
            .await
            .unwrap(),
        );
        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(PinCheckingAdapter {
            inner: ScriptedAdapter::new(plain_text_script()).with_kind(kind),
            expected_pin: expected_pin.clone(),
        }));
        let mut runtime =
            CodeRuntime::with_registry(Arc::clone(&db), data_dir.path().to_path_buf(), registry);
        runtime.host.data_dir = Some(data_dir.path().to_path_buf());
        let broker = Arc::new(AvailableNodeBroker {
            ensure_calls: AtomicUsize::new(0),
            root: node_root,
        });
        runtime.host_tool_broker = Some(broker.clone());

        let owner = OwnerId::new("alice").unwrap();
        let first = saved_session(&owner, kind);
        let second = saved_session(&owner, kind);
        insert_session(&db, &first).await.unwrap();
        insert_session(&db, &second).await.unwrap();

        runtime.recover().await.unwrap();

        assert!(expected_pin.is_file());
        assert_eq!(
            broker.ensure_calls.load(Ordering::Relaxed),
            1,
            "sessions of one kind share one recovery install"
        );
        assert!(runtime.has_worker(first.id));
        assert!(runtime.has_worker(second.id));
    }
}
