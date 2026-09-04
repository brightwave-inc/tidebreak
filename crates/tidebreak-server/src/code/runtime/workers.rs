//! Per-session workers: spawn, attach, shut down, and wake.

use super::*;

/// How often a deferred worker resync re-reads a running session.
const DEFERRED_RESYNC_POLL: Duration = Duration::from_secs(2);
/// How long a deferred worker resync waits for a turn to end before it
/// gives up and lets the next fault surface the old binary.
const DEFERRED_RESYNC_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);

impl CodeRuntime {
    /// The `PATH` a machine session's child runs with when this machine
    /// lends forge credentials: the session's private `bin` first, holding
    /// a `gh` wrapper that borrows the credential per call, then the probe's
    /// own path. Best effort: a machine without `gh`, a repository with no
    /// origin, or a private root that refuses the write leaves the path
    /// alone and the child's `gh` behaves as it always did.
    async fn gh_shim_path(
        &self,
        private_root: &crate::code::scratch::ScratchRoot,
        workspace: Option<&tidebreak_core::CodeWorkspace>,
        probe_env: &[(std::ffi::OsString, std::ffi::OsString)],
        loopback_base: &str,
        owner: &OwnerId,
    ) -> Option<String> {
        let workspace = workspace?;
        let repo = self.get_repo(owner, workspace.repo_id).await.ok()?;
        let origin_host = repo.origin_host?;
        let real = crate::code::gh::observe_gh(self.gh_search_path_owned().as_deref())
            .await
            .binary?;
        let script = crate::code::harness_llm::gh_shim_script(&real, loopback_base, &origin_host);
        let shim = match private_root
            .publish_executable(
                std::ffi::OsStr::new("bin"),
                std::ffi::OsStr::new("gh"),
                script.as_bytes(),
            )
            .await
        {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(%error, "the session's gh wrapper was not written");
                return None;
            }
        };
        let bin = shim.parent()?.to_path_buf();
        let prior = probe_env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(&prior));
        Some(
            std::env::join_paths(paths)
                .ok()?
                .to_string_lossy()
                .into_owned(),
        )
    }

    pub(super) fn wake_all_workers(&self) {
        for handle in self.workers.lock().expect("code workers").values() {
            wake_queue(handle);
        }
    }

    /// Ask a superseded worker to stop, and wait for its command receiver to
    /// close — that is the last thing the worker does. One `Shutdown` is
    /// enough; resending would only replay `interrupt` into an engine that is
    /// already stopping. The wait is bounded so a wedged engine cannot hold
    /// an archive or a reap open.
    ///
    /// Returns whether the worker confirmed it was gone. That answer matters
    /// to the caller: a worker that outlived its grace still holds this
    /// workspace's turn lock, so anything keyed on "the checkout is now
    /// unowned" has to stay put.
    pub(super) async fn shut_down_worker(id: SessionId, handle: WorkerHandle) -> bool {
        const GRACE: std::time::Duration = std::time::Duration::from_secs(5);
        let commands = handle.commands.clone();
        drop(handle);
        // Send and wait under one deadline: a worker that has stopped draining
        // its commands would otherwise park the send itself once the channel
        // filled up.
        let stopped = tokio::time::timeout(GRACE, async {
            let _ = commands.send(WorkerCommand::Shutdown).await;
            commands.closed().await;
        })
        .await
        .is_ok();
        if !stopped {
            tracing::warn!(
                session = %id,
                "code-mode: session worker did not stop in time; continuing without it"
            );
        }
        stopped
    }

    pub async fn attach_and_spawn_worker(&self, session: Session) -> Result<Session, ServerError> {
        let mut session = session;
        let workspace = self.session_workspace(&session).await?;
        let adapter = self.adapter(session.harness_kind)?;
        // Cached, so the probe `create_session` already paid for is not paid
        // again on the way into the worker.
        let probe = self.probe(adapter.as_ref()).await;
        if !probe.found {
            return Err(ServerError::unprocessable_kind(
                "harness_not_found",
                format!("{} is not installed", session.harness_kind),
            ));
        }
        if session.reasoning_effort.is_some() || session.fast_mode {
            let selected = self
                .selected_model_capabilities_for_owner(
                    &session.owner,
                    adapter.as_ref(),
                    &probe,
                    session.model.as_deref(),
                )
                .await;
            let mut next = SessionExecutionSettings::from(&session);
            selected.deactivate_unsupported(&mut next);
            if next != SessionExecutionSettings::from(&session) {
                session =
                    replace_session_execution_settings(&self.db, &session.owner, &session, &next)
                        .await?
                        .ok_or_else(|| {
                            ServerError::conflict_kind(
                                "session_settings_changed",
                                "the session settings changed before its worker could attach",
                            )
                        })?;
            }
        }
        let binary = match probe.binary_path.clone() {
            Some(binary) => Some(binary),
            // An in-process engine has no binary to resolve.
            None if session.harness_kind.is_in_process() => None,
            None => {
                return Err(ServerError::unprocessable_kind(
                    "harness_not_found",
                    format!("{} has no path", session.harness_kind),
                ))
            }
        };
        let attached = attach_engine(
            &self.db,
            &self.bus,
            session.id,
            session.harness_kind,
            probe.version.clone().or(session.harness_version.clone()),
            None,
        )
        .await
        .map_err(map_worker)?;
        let sink = crate::code::session_worker::sink_for(
            self.db.clone(),
            self.bus.clone(),
            session.owner.clone(),
            session.id,
            attached.spawn_epoch,
            session.harness_kind,
            self.harness_llm.is_some()
                && crate::code::harness_llm::relay_covered(session.harness_kind),
            None,
            attached.subagents.clone(),
            self.gh_search_path_owned(),
            self.recap_hook(),
            self.rewrite_hook(),
            self.memory_capture_hook(),
            self.hot_pull_requests(),
        );
        // An in-process engine parks its approvals on the adapter's own
        // channel; the loopback MCP prompt is for engines that speak MCP.
        let approval = if session.harness_kind.is_in_process() {
            None
        } else {
            self.approval_channel(
                &attached.owner,
                attached.id,
                attached.spawn_epoch,
                session.permission_mode,
            )
        };

        // Mint a browser channel only when both halves are present: the
        // native BrowserRuntime (the desktop adapter) and the trusted
        // bridge executable (the CLI sidecar). If either is absent, browser
        // stays None — no browser tools are advertised or injected, and the
        // session works exactly as before the browser channel existed.
        let browser = match (
            self.browser_runtime.as_ref(),
            self.browser_bridge_command.as_ref(),
            session.workspace_id,
        ) {
            (Some(runtime), Some(bridge), Some(workspace)) => {
                let browser_subject = BrowserSubject {
                    owner: session.owner.clone(),
                    workspace,
                    session: session.id,
                };
                Some(
                    self.browser_tokens
                        .issue_with_semantic_actions(
                            browser_subject,
                            bridge,
                            runtime.supports_semantic_actions(),
                        )
                        .map_err(ServerError::internal)?,
                )
            }
            _ => None,
        };

        let private_root = match &workspace {
            Some(workspace) => crate::code::scratch::workspace_root(&self.data_dir, workspace.id),
            None => crate::code::scratch::session_root(&self.data_dir, session.id),
        }
        .map_err(|err| ServerError::internal(format!("could not open private storage: {err}")))?;

        // On a gateway-authenticated machine, point the engine's own
        // inference at this server's relay (decision 71): a per-session key
        // stands in for provider credentials the hosted image does not have.
        let (extra_argv, extra_env, relay_key_env) = match self.harness_llm.as_ref() {
            // An in-process engine resolves inference through the server
            // itself; there is no child to point at the relay.
            Some(relay) if !session.harness_kind.is_in_process() => {
                let base = self
                    .loopback_base
                    .lock()
                    .expect("loopback base")
                    .clone()
                    .ok_or_else(|| {
                        ServerError::internal("harness LLM relay: loopback base not set")
                    })?;
                let key = relay.issue(crate::code::harness_llm::HarnessLlmSubject {
                    owner: session.owner.clone(),
                    session: session.id,
                });
                let (argv, mut env) =
                    crate::code::harness_llm::spawn_wiring(session.harness_kind, &base, &key);
                // A repository session's own git borrows the person's forge
                // credential through the loopback route under the same key,
                // so the harness's shell can push what it made. A machine
                // that lends none leaves git as it found it.
                if workspace.is_some() && self.git_credentials.is_some() {
                    env.extend(crate::code::harness_llm::git_credential_wiring(&base));
                    if let Some(path) = self
                        .gh_shim_path(
                            &private_root,
                            workspace.as_ref(),
                            &probe.env,
                            &base,
                            &session.owner,
                        )
                        .await
                    {
                        env.push(("PATH".to_owned(), path));
                    }
                }
                (
                    argv,
                    env,
                    Some(crate::code::harness_llm::RELAY_KEY_ENV.to_owned()),
                )
            }
            _ => (Vec::new(), Vec::new(), None),
        };

        let spec = SessionSpec {
            owner: session.owner.clone(),
            session_id: session.id,
            // With no workspace the private root doubles as the working
            // directory; the in-process engine keeps its own scratch there.
            worktree: match &workspace {
                Some(workspace) => PathBuf::from(&workspace.worktree_path),
                None => private_root.path().to_path_buf(),
            },
            allowed_read_roots: vec![private_root.path().to_path_buf()],
            permission_mode: session.permission_mode,
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort,
            fast_mode: session.fast_mode,
            resume_ref: session.harness_resume_ref.clone(),
            extra_argv,
            extra_env,
            relay_key_env,
            env: probe.env.clone(),
            approval,
            binary: binary.clone(),
            sink: sink.clone() as Arc<dyn HarnessEventSink>,
            browser,
        };
        let mut attached = attached;
        let engine = match adapter.launch(spec).await {
            Ok(engine) => engine,
            Err(HarnessError::ResumeLost(detail)) => {
                self.revoke_worker_channels(session.id);
                // The engine refused the stored resume ref. Fence with a
                // reason the UI can explain — the fence drops the dead ref, so
                // a reap re-attaches with a fresh engine session.
                recovery::fence_session(
                    &self.db,
                    &self.bus,
                    &mut attached,
                    FenceReason::ResumeLost {
                        detail: detail.clone(),
                    },
                )
                .await?;
                return Err(ServerError::conflict_kind(
                    "session_resume_lost",
                    format!("the engine no longer has this session: {detail}"),
                ));
            }
            Err(err) => {
                self.revoke_worker_channels(session.id);
                return Err(ServerError::internal(format!(
                    "failed to launch engine session: {err}"
                )));
            }
        };
        attached.child_pid = engine.child_pid();
        attached.child_process_identity = attached.child_pid.and_then(|pid| {
            tidebreak_harness::spawned_process_identity(pid).or_else(|| {
                tidebreak_harness::current_process_identity(pid)
                    .ok()
                    .flatten()
            })
        });
        if let Some(resume) = engine.resume_ref().or(session.harness_resume_ref.clone()) {
            attached.harness_resume_ref = Some(resume);
        }
        crate::code::attention::persist_session(&self.db, &self.bus, &attached).await?;
        let mut handle = spawn_session_worker(
            attached.clone(),
            engine,
            sink,
            AttachmentStore {
                blobs: Some(self.blobs.clone()),
                private_root,
                // Only an engine that states image input takes the bytes on
                // its own protocol. The rest receive absolute private paths.
                engine_reads_images: adapter.capabilities(&probe).image_input
                    == CapLevel::Supported,
            },
            // A session with no workspace shares its working directory with
            // nothing, so it takes a lock of its own.
            match attached.workspace_id {
                Some(workspace_id) => self.worktree_turn_lock(workspace_id),
                None => Arc::new(tokio::sync::Mutex::new(())),
            },
            self.update_quiesce.subscribe(),
        );
        handle.binary = binary;
        self.workers
            .lock()
            .expect("code workers")
            .insert(session.id, handle);
        let pending = list_approvals(
            &self.db,
            &attached.owner,
            Some(ApprovalState::Pending),
            Some(attached.id),
        )
        .await?;
        if !pending.is_empty() {
            crate::code::attention::replace_attention(
                &mut attached,
                Attention::needs_you("an approval is waiting", AttentionSource::Structured),
                false,
            );
            crate::code::attention::persist_session(&self.db, &self.bus, &attached).await?;
        }
        Ok(attached)
    }

    /// Whether a worker is attached to the session right now.
    pub fn has_worker(&self, id: SessionId) -> bool {
        self.workers.lock().expect("code workers").contains_key(&id)
    }

    /// Move every idle live worker of `kinds` onto the binary the update
    /// channel now selects.
    ///
    /// A worker copies the probe's binary path into its launch plan once, so
    /// a managed install that lands a newer release, or a channel flip back
    /// to the pin, leaves an attached session driving the old file — and a
    /// retry of the turn that failed on a version floor hits it again. Each
    /// idle worker still on another file is stopped and respawned through
    /// the fresh probe, the same swap a settings change makes. A worker with
    /// a turn in flight is not stopped — that would abandon the work — but
    /// it is not forgotten either: a deferred pass watches the session and
    /// makes the same swap once the turn ends, before the next prompt is
    /// dispatched through the old file.
    ///
    /// Returns the sessions whose worker moved on this pass.
    pub async fn resync_workers_to_selected_binaries(
        self: &Arc<Self>,
        kinds: &[HarnessKind],
    ) -> Vec<SessionId> {
        let mut moved = Vec::new();
        for kind in kinds {
            // A declared binary never moves, and a host with no data
            // directory selects no managed install.
            if kind.is_in_process()
                || self.host.data_dir.is_none()
                || self.host.declared(*kind).is_some()
            {
                continue;
            }
            let Some(selected) = self.selected_harness(*kind).await else {
                continue;
            };
            let stale: Vec<(SessionId, i64, OwnerId)> = self
                .workers
                .lock()
                .expect("code workers")
                .iter()
                .filter(|(_, handle)| {
                    handle
                        .binary
                        .as_deref()
                        .is_some_and(|binary| binary != selected.binary)
                })
                .map(|(id, handle)| (*id, handle.spawn_epoch, handle.sink.owner().clone()))
                .collect();
            for (id, spawn_epoch, owner) in stale {
                let Ok(session) = self.get_session(&owner, id).await else {
                    continue;
                };
                if session.harness_kind != *kind || session.spawn_epoch != spawn_epoch {
                    continue;
                }
                if session.lifecycle == SessionLifecycle::Running {
                    self.defer_worker_resync(*kind, id, owner);
                    continue;
                }
                let Some(handle) = self.take_worker_for_epoch(id, spawn_epoch) else {
                    continue;
                };
                self.revoke_worker_channels(id);
                Self::shut_down_worker(id, handle).await;
                let respawned = match self.get_session(&owner, id).await {
                    Ok(session) => self.attach_and_spawn_worker(session).await,
                    Err(error) => Err(error),
                };
                match respawned {
                    Ok(_) => moved.push(id),
                    Err(error) => tracing::warn!(
                        session = %id,
                        %kind,
                        error = ?error,
                        "could not respawn the session on the selected engine binary"
                    ),
                }
            }
        }
        moved
    }

    /// Finish a worker swap once the session's turn in flight ends.
    ///
    /// Turn completion only marks the row idle; the next prompt reuses the
    /// worker as it stands. So a session that was running when the selected
    /// binary moved is watched here, and re-checked through the same resync
    /// the moment it is no longer running. One watcher per session: a second
    /// install or channel flip while one is pending changes what the resync
    /// will select, not whether it runs.
    fn defer_worker_resync(self: &Arc<Self>, kind: HarnessKind, id: SessionId, owner: OwnerId) {
        if !self
            .deferred_resyncs
            .lock()
            .expect("deferred resyncs")
            .insert(id)
        {
            return;
        }
        tracing::info!(
            session = %id,
            %kind,
            "a turn is in flight; the session moves to the selected engine binary when it ends"
        );
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let deadline = TokioInstant::now() + DEFERRED_RESYNC_DEADLINE;
            let outcome = loop {
                tokio::time::sleep(DEFERRED_RESYNC_POLL).await;
                match runtime.get_session(&owner, id).await {
                    Ok(session) if session.lifecycle != SessionLifecycle::Running => {
                        break Ok(());
                    }
                    Ok(_) if TokioInstant::now() < deadline => continue,
                    Ok(_) => break Err("the turn did not end before the deadline"),
                    Err(_) => break Err("the session could not be read"),
                }
            };
            runtime
                .deferred_resyncs
                .lock()
                .expect("deferred resyncs")
                .remove(&id);
            match outcome {
                Ok(()) => {
                    runtime.resync_workers_to_selected_binaries(&[kind]).await;
                }
                Err(reason) => tracing::warn!(
                    session = %id,
                    %kind,
                    reason,
                    "gave up moving the session to the selected engine binary"
                ),
            }
        });
    }

    pub(super) fn require_worker(&self, id: SessionId) -> Result<WorkerHandle, ServerError> {
        self.workers
            .lock()
            .expect("code workers")
            .get(&id)
            .map(|handle| WorkerHandle {
                spawn_epoch: handle.spawn_epoch,
                binary: handle.binary.clone(),
                commands: handle.commands.clone(),
                queue: handle.queue.clone(),
                sink: handle.sink.clone(),
                approval_decisions: handle.approval_decisions.clone(),
                abort: handle.abort.clone(),
            })
            .ok_or_else(|| {
                ServerError::conflict_kind(
                    "session_worker_missing",
                    "no live worker is attached to this session",
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::harness_release::test_support::write_install;
    use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
    use tidebreak_core::db::DbStore;

    /// A runtime whose only engine is the scripted adapter, on a host with a
    /// data directory so the update channel selects managed installs.
    async fn scripted_runtime(data_dir: &Path) -> CodeRuntime {
        let db = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            data_dir.join("code.db").display()
        ))
        .await
        .expect("db");
        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
        let mut runtime =
            CodeRuntime::with_registry(Arc::new(db), data_dir.to_path_buf(), registry);
        runtime.host.data_dir = Some(data_dir.to_path_buf());
        runtime
    }

    fn workspaceless_session(owner: &OwnerId, harness: HarnessKind) -> Session {
        Session {
            id: SessionId::new(),
            owner: owner.clone(),
            workspace_id: None,
            kind: SessionKind::Interactive,
            harness_kind: harness,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: SessionLifecycle::Created,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
            execution_location: tidebreak_core::ExecutionLocation::Machine,
        }
    }

    /// A worker still on another file than the channel selects is respawned
    /// when it is idle, left to finish when a turn is in flight, and moved
    /// once that turn ends.
    #[tokio::test]
    async fn a_stale_idle_worker_is_respawned_and_a_running_one_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let kind = HarnessKind::ClaudeCode;
        let pin = tidebreak_harness::pin_for(kind).unwrap();
        write_install(tmp.path(), kind, pin.version);
        let runtime = Arc::new(scripted_runtime(tmp.path()).await);
        let owner = OwnerId::new("alice").unwrap();
        let session = workspaceless_session(&owner, kind);
        insert_session(&runtime.db, &session).await.unwrap();
        // The scripted probe names `/scripted/engine`, which no channel ever
        // selects, so this worker is stale from the moment a managed install
        // is on disk — exactly the shape an Update leaves behind.
        let attached = runtime.attach_and_spawn_worker(session).await.unwrap();
        let first_epoch = attached.spawn_epoch;

        let moved = runtime.resync_workers_to_selected_binaries(&[kind]).await;
        assert_eq!(moved, vec![attached.id]);
        let session = runtime.get_session(&owner, attached.id).await.unwrap();
        assert!(session.spawn_epoch > first_epoch, "the worker respawned");
        assert!(runtime.require_worker(attached.id).is_ok());

        let mut running = session.clone();
        running.lifecycle = SessionLifecycle::Running;
        crate::code::attention::persist_session(&runtime.db, &runtime.bus, &running)
            .await
            .unwrap();
        assert!(runtime
            .resync_workers_to_selected_binaries(&[kind])
            .await
            .is_empty());
        assert_eq!(
            runtime
                .get_session(&owner, attached.id)
                .await
                .unwrap()
                .spawn_epoch,
            session.spawn_epoch
        );

        // Once the turn ends, the deferred pass makes the swap on its own.
        let mut idle = running.clone();
        idle.lifecycle = SessionLifecycle::Idle;
        crate::code::attention::persist_session(&runtime.db, &runtime.bus, &idle)
            .await
            .unwrap();
        let moved_epoch = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let now = runtime.get_session(&owner, attached.id).await.unwrap();
                if now.spawn_epoch > session.spawn_epoch {
                    break now.spawn_epoch;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("the deferred resync moves the worker after the turn ends");
        assert!(moved_epoch > session.spawn_epoch);
        assert!(runtime.require_worker(attached.id).is_ok());

        // Another engine's install never touches this worker.
        assert!(runtime
            .resync_workers_to_selected_binaries(&[HarnessKind::Codex])
            .await
            .is_empty());
    }
}
