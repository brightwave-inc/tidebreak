//! Per-session workers: spawn, attach, shut down, and wake.

use super::*;

impl CodeRuntime {
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
    pub(super) async fn shut_down_worker(id: CodeSessionId, handle: WorkerHandle) -> bool {
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

    pub(super) async fn attach_and_spawn_worker(
        &self,
        session: CodeSession,
    ) -> Result<CodeSession, ServerError> {
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
            let mut next = CodeSessionExecutionSettings::from(&session);
            selected.deactivate_unsupported(&mut next);
            if next != CodeSessionExecutionSettings::from(&session) {
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
                let (argv, env) =
                    crate::code::harness_llm::spawn_wiring(session.harness_kind, &base, &key);
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
            binary,
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
        let handle = spawn_session_worker(
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
        self.workers
            .lock()
            .expect("code workers")
            .insert(session.id, handle);
        let pending = list_approvals(
            &self.db,
            &attached.owner,
            Some(CodeApprovalState::Pending),
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

    pub(super) fn require_worker(&self, id: CodeSessionId) -> Result<WorkerHandle, ServerError> {
        self.workers
            .lock()
            .expect("code workers")
            .get(&id)
            .map(|handle| WorkerHandle {
                spawn_epoch: handle.spawn_epoch,
                commands: handle.commands.clone(),
                queue: handle.queue.clone(),
                sink: handle.sink.clone(),
                approval_decisions: handle.approval_decisions.clone(),
            })
            .ok_or_else(|| {
                ServerError::conflict_kind(
                    "session_worker_missing",
                    "no live worker is attached to this session",
                )
            })
    }
}
