//! Session rows: creation, attachments, listing, turn history, attention, and debugging reads.

use super::*;

impl CodeRuntime {
    pub async fn create_session(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        self.create_session_of_kind(
            owner,
            workspace_id,
            SessionKind::Interactive,
            harness,
            settings,
        )
        .await
    }

    /// Shared create path for interactive sessions and watch sessions.
    ///
    /// A workspace holds any number of interactive sessions and at most one
    /// watch session, so the guard below covers watch only. The worktree they
    /// share is protected by the per-workspace turn lock rather than by a cap
    /// on conversations; see record 55.
    pub async fn create_session_of_kind(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        kind: SessionKind,
        harness: HarnessKind,
        NewSessionSettings {
            permission_mode,
            model,
            reasoning_effort,
            fast_mode,
            permission_mode_ceiling,
        }: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        if harness.is_in_process() {
            return Err(ServerError::conflict_kind(
                "harness_needs_no_workspace",
                "the internal engine hosts conversations without a workspace; create one without a workspace instead",
            ));
        }
        let lifecycle = self.workspace_lifecycle_lock(workspace_id);
        let _lifecycle_guard = lifecycle.lock().await;
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; create a remote session on it",
            ));
        }
        if kind == SessionKind::Watch {
            let existing = list_sessions_for_workspace(&self.db, owner, workspace_id).await?;
            if existing.iter().any(|session| {
                session.lifecycle != SessionLifecycle::Ended && session.kind == SessionKind::Watch
            }) {
                return Err(ServerError::conflict_kind(
                    "session_exists",
                    "this workspace already has an active watch session",
                ));
            }
        }
        let adapter = self.adapter(harness)?;
        #[cfg(not(any(test, feature = "test-support")))]
        {
            // The warm install the dialog starts usually got here first, in
            // which case this is a marker read. It stays on the create path
            // regardless: correctness must not depend on the warm path having
            // run, and a pin installed here is serialized against that one.
            // Skip when a CLI e2e has replaced this kind with the scripted
            // engine: that binary has no pin and must not try to download one.
            let skip_pin = {
                #[cfg(debug_assertions)]
                {
                    crate::scripted_harness::env_is_set()
                }
                #[cfg(not(debug_assertions))]
                {
                    false
                }
            };
            if !skip_pin && !harness.is_in_process() {
                match self.ensure_harness(harness, false, false).await {
                    Ok(installed) => {
                        self.record_pin_install(harness, Ok(()));
                        self.invalidate_moved_probe(harness, &installed.binary);
                    }
                    Err(err) => {
                        self.record_pin_install(harness, Err(err.clone()));
                        return Err(ServerError::unprocessable_kind(
                            "harness_not_found",
                            format!("{harness} could not be installed: {err}"),
                        ));
                    }
                }
            }
        }
        let probe = self.probe_for_session_create(adapter.as_ref()).await;
        if !probe.found {
            return Err(ServerError::unprocessable_kind(
                "harness_not_found",
                format!(
                    "{harness} is not installed{}",
                    if probe.stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", probe.stderr)
                    }
                ),
            ));
        }
        let caps = adapter.capabilities(&probe);
        refuse_ceiling_with_no_offered_mode(permission_mode_ceiling, harness, &caps)?;
        if let Some(ceiling) = permission_mode_ceiling {
            if permission_mode > ceiling {
                return Err(ServerError::conflict_kind(
                    "permission_mode_locked",
                    format!(
                        "permission mode `{}` exceeds the maximum this managed profile allows (`{}`)",
                        permission_mode.as_str(),
                        ceiling.as_str()
                    ),
                ));
            }
        }
        refuse_unhonored_mode(harness, permission_mode, &caps)?;
        if probe.binary_path.is_none() && !harness.is_in_process() {
            return Err(ServerError::unprocessable_kind(
                "harness_not_found",
                format!("{harness} has no path"),
            ));
        }
        self.refuse_signed_out_harness(harness, &probe)?;
        let execution_settings = SessionExecutionSettings {
            model: normalize_model(model),
            reasoning_effort,
            fast_mode,
        };
        if execution_settings.reasoning_effort.is_some() || execution_settings.fast_mode {
            let selected = self
                .selected_model_capabilities_for_owner(
                    owner,
                    adapter.as_ref(),
                    &probe,
                    execution_settings.model.as_deref(),
                )
                .await;
            Self::validate_execution_settings(harness, &execution_settings, &selected)?;
        }
        let session = Session {
            id: SessionId::new(),
            owner: owner.clone(),
            workspace_id: Some(workspace_id),
            kind,
            harness_kind: harness,
            harness_version: probe.version.clone(),
            harness_resume_ref: None,
            permission_mode,
            model: execution_settings.model,
            reasoning_effort: execution_settings.reasoning_effort,
            fast_mode: execution_settings.fast_mode,
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
        };
        insert_session(&self.db, &session).await?;
        // Pin where this session starts, before it can take a turn. Sessions
        // share the worktree (record 55), so without a baseline the first
        // turn's diff is the whole worktree against the base branch — a
        // sibling's edits included.
        if let Err(err) = record_session_baseline(
            Path::new(&workspace.worktree_path),
            workspace.id,
            session.id,
        )
        .await
        {
            tracing::warn!(
                session = %session.id,
                workspace = %workspace.id,
                error = %err,
                "could not record the session baseline; its first turn diffs against the base ref"
            );
        }
        self.attach_and_spawn_worker(session).await
    }

    /// Create a conversation with no workspace, hosted by the in-process
    /// engine (decision 0048 step 5).
    ///
    /// The workspace-bound create path probes an installed engine and pins
    /// a checkout; this one needs neither. Everything after the row — the
    /// worker, the journal, approvals — is the same machinery.
    pub async fn create_internal_session(
        &self,
        owner: &OwnerId,
        NewSessionSettings {
            permission_mode,
            model,
            reasoning_effort,
            fast_mode,
            permission_mode_ceiling,
        }: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        let harness = HarnessKind::Internal;
        let adapter = self.adapter(harness)?;
        let probe = self.probe_for_session_create(adapter.as_ref()).await;
        let caps = adapter.capabilities(&probe);
        refuse_ceiling_with_no_offered_mode(permission_mode_ceiling, harness, &caps)?;
        if let Some(ceiling) = permission_mode_ceiling {
            if permission_mode > ceiling {
                return Err(ServerError::conflict_kind(
                    "permission_mode_locked",
                    format!(
                        "permission mode `{}` exceeds the maximum this managed profile allows (`{}`)",
                        permission_mode.as_str(),
                        ceiling.as_str()
                    ),
                ));
            }
        }
        refuse_unhonored_mode(harness, permission_mode, &caps)?;
        let execution_settings = SessionExecutionSettings {
            model: normalize_model(model),
            reasoning_effort,
            fast_mode,
        };
        if execution_settings.fast_mode {
            return Err(ServerError::unprocessable_kind(
                "fast_mode_unsupported",
                "the internal engine has no fast mode",
            ));
        }
        let session = Session {
            id: SessionId::new(),
            owner: owner.clone(),
            workspace_id: None,
            kind: SessionKind::Interactive,
            harness_kind: harness,
            harness_version: probe.version.clone(),
            harness_resume_ref: None,
            permission_mode,
            model: execution_settings.model,
            reasoning_effort: execution_settings.reasoning_effort,
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
        };
        insert_session(&self.db, &session).await?;
        self.attach_and_spawn_worker(session).await
    }

    pub async fn get_session(
        &self,
        owner: &OwnerId,
        id: SessionId,
    ) -> Result<Session, ServerError> {
        get_session(&self.db, owner, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("session {id} not found")))
    }

    /// Resolve requested attachments for one session, or refuse.
    ///
    /// Takes the owner and session because publication is per-session
    /// authority. Resolving without them — as this did — could only check that
    /// the bytes existed somewhere, which is not the same question.
    pub async fn resolve_turn_attachments(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        requested: &[(uuid::Uuid, String)],
    ) -> Result<Vec<tidebreak_core::ImageRef>, ServerError> {
        if requested.len() > tidebreak_core::context::MAX_HYDRATED_IMAGES {
            return Err(ServerError::bad_request(format!(
                "a turn may carry at most {} image attachments",
                tidebreak_core::context::MAX_HYDRATED_IMAGES
            )));
        }
        let mut resolved = Vec::with_capacity(requested.len());
        let mut resolved_bytes = 0_u64;
        for (blob_id, media_type) in requested {
            if blob_id.is_nil() {
                return Err(ServerError::bad_request(
                    "attachment blob_id must not be nil",
                ));
            }
            let media_type = parse_turn_media_type(media_type).ok_or_else(|| {
                ServerError::bad_request(format!("unsupported attachment media type {media_type}"))
            })?;
            // Publication is the authority, not the blob's existence. The blob
            // store is content-addressed and owner-blind, so checking only
            // that the bytes are present would let any known id be bound into
            // this session and read back through its own image route. An
            // unpublished id is refused as not-found, so the failure cannot
            // confirm the blob exists somewhere else.
            let unpublished = || {
                ServerError::bad_request(format!(
                    "attachment blob {blob_id} was not published to session {session_id}"
                ))
            };
            let published = tidebreak_core::db::code::get_published_session_image(
                &self.db, owner, session_id, *blob_id,
            )
            .await?
            .ok_or_else(unpublished)?;
            if published.media_type != media_type {
                return Err(ServerError::bad_request(format!(
                    "attachment blob {blob_id} was published as {}",
                    published.media_type
                )));
            }
            // Re-derive from the bytes, the way chat's resolution does: an id
            // is a content address, so bytes that no longer hash back to it,
            // or that no longer match the reserved descriptor, are unresolved
            // rather than merely mismatched.
            let bytes = self
                .blobs
                .get(*blob_id)
                .await
                .map_err(|err| ServerError::internal(format!("blob read: {err}")))?
                .ok_or_else(unpublished)?;
            let image = crate::image_attachment::inspect_image_bytes(&bytes)?;
            if image.blob_id != *blob_id || image != published {
                return Err(unpublished());
            }
            resolved_bytes = resolved_bytes.saturating_add(image.byte_len);
            if resolved_bytes
                > u64::try_from(tidebreak_core::context::MAX_HYDRATED_IMAGE_BYTES)
                    .expect("the image byte limit fits in u64")
            {
                return Err(ServerError::bad_request(format!(
                    "turn image attachments may total at most {} bytes",
                    tidebreak_core::context::MAX_HYDRATED_IMAGE_BYTES
                )));
            }
            resolved.push(image);
        }
        Ok(resolved)
    }

    pub async fn set_attention(
        &self,
        owner: &OwnerId,
        id: SessionId,
        clear: bool,
        note: Option<String>,
    ) -> Result<Session, ServerError> {
        let _ = self.get_session(owner, id).await?;
        crate::code::attention::user_set_attention(&self.db, &self.bus, owner, id, clear, note)
            .await
            .map_err(ServerError::from)
    }

    pub async fn mark_session_viewed(
        &self,
        owner: &OwnerId,
        id: SessionId,
    ) -> Result<(), ServerError> {
        crate::code::attention::mark_viewed(&self.db, &self.bus, owner, id)
            .await
            .map_err(ServerError::from)
    }

    /// End one session: mark the row ended, stop its worker, and re-assert.
    ///
    /// The same steps [`Self::end_workspace_sessions`] takes per session,
    /// for callers that must end a single session (the watch path) without
    /// touching the workspace's other sessions.
    pub(in crate::code) async fn end_session_row(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
    ) -> Result<(), ServerError> {
        let Some(mut session) = get_session(&self.db, owner, session_id).await? else {
            return Ok(());
        };
        if session.lifecycle == SessionLifecycle::Ended {
            return Ok(());
        }
        if let Ok(Some(workspace)) = self.session_workspace(&session).await {
            if workspace.is_remote() {
                self.cancel_remote_sandbox(&session).await;
            }
        }
        let handle = self
            .workers
            .lock()
            .expect("code workers")
            .remove(&session.id);
        let decision_gate = handle
            .as_ref()
            .map(|handle| handle.approval_decisions.clone());
        let _decision_guard = match decision_gate {
            Some(gate) => Some(gate.lock_owned().await),
            None => None,
        };
        self.revoke_browser_session(&session);
        session.lifecycle = SessionLifecycle::Ended;
        session.child_pid = None;
        session.child_process_identity = None;
        session.fence_reason = None;
        crate::code::attention::persist_session(&self.db, &self.bus, &session).await?;
        let stopped = match handle {
            Some(handle) => Self::shut_down_worker(session.id, handle).await,
            None => true,
        };
        let mut current = self.get_session(owner, session.id).await?;
        current.lifecycle = SessionLifecycle::Ended;
        current.child_pid = None;
        current.child_process_identity = None;
        current.fence_reason = None;
        if !crate::code::attention::persist_session(&self.db, &self.bus, &current).await? {
            return Err(ServerError::conflict_kind(
                "session_not_ended",
                "the session did not stay ended after the worker stopped",
            ));
        }
        if stopped {
            crate::code::approval_sweep::abandon_for_restart(
                &self.db,
                &self.bus,
                owner,
                current.id,
                current.spawn_epoch,
            )
            .await;
        } else {
            crate::code::approval_sweep::abandon_for_ended_session(
                &self.db,
                &self.bus,
                owner,
                current.id,
                current.spawn_epoch,
            )
            .await;
        }
        // Nothing will ever promote an ended session's queued rows; retract
        // them so the queue does not read as pending work forever.
        if let Err(error) =
            tidebreak_core::db::code::delete_session_queued_turns(&self.db, owner, current.id).await
        {
            tracing::warn!(
                session = %current.id,
                error = %error,
                "could not clear the ended session's queued turns"
            );
        }
        Ok(())
    }

    pub async fn list_sessions(&self, owner: &OwnerId) -> Result<Vec<Session>, ServerError> {
        Ok(list_sessions(&self.db, owner).await?)
    }

    pub async fn list_workspace_sessions(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<Session>, ServerError> {
        let _ = self.get_workspace(owner, workspace_id).await?;
        Ok(list_sessions_for_workspace(&self.db, owner, workspace_id).await?)
    }

    /// The owner's sessions that bind no workspace, newest first.
    pub async fn list_internal_sessions(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<Session>, ServerError> {
        Ok(tidebreak_core::db::code::list_sessions(&self.db, owner)
            .await?
            .into_iter()
            .filter(|session| session.workspace_id.is_none())
            .collect())
    }

    /// The external bindings behind `session_ids`, for provenance display.
    /// Sessions the desktop created have none and are simply absent.
    pub async fn external_bindings_for_sessions(
        &self,
        owner: &OwnerId,
        session_ids: &[SessionId],
    ) -> Result<Vec<tidebreak_core::CodeExternalBinding>, ServerError> {
        Ok(
            tidebreak_core::db::code::list_external_bindings_for_sessions(
                &self.db,
                owner,
                session_ids,
            )
            .await?,
        )
    }

    pub async fn list_session_turns(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
    ) -> Result<Vec<Turn>, ServerError> {
        let _ = self.get_session(owner, session_id).await?;
        Ok(list_turns(&self.db, owner, session_id).await?)
    }

    pub async fn list_turn_metrics(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<tidebreak_core::db::code::TurnMetric>, ServerError> {
        Ok(tidebreak_core::db::code::list_turn_metrics(&self.db, owner).await?)
    }

    pub async fn list_pull_request_facts(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<tidebreak_core::CodePullRequestFact>, ServerError> {
        Ok(tidebreak_core::db::code::list_pull_request_facts(&self.db, owner).await?)
    }

    pub async fn list_pull_request_attributions(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<tidebreak_core::CodePullRequestAttribution>, ServerError> {
        Ok(tidebreak_core::db::code::list_pull_request_attributions(&self.db, owner).await?)
    }

    pub async fn session_debug(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
    ) -> Result<
        (
            Session,
            Vec<Turn>,
            Vec<tidebreak_core::code::SequencedEvent>,
        ),
        ServerError,
    > {
        let session = self.get_session(owner, session_id).await?;
        let turns = list_turns(&self.db, owner, session_id).await?;
        let events = list_events(&self.db, owner, session_id, 0, MAX_REPLAY_EVENTS).await?;
        Ok((session, turns, events.events))
    }

    /// Write one fork of this session — the condensed transcript plus a full
    /// record per turn — into private storage, for a child agent to read.
    ///
    /// `at_turn` forks at the end of that turn; `None` forks at the newest.
    /// The caller creates the child and names the absolute path in its first
    /// message. Git cannot index the transcript or its attachments.
    pub async fn fork_transcript(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        at_turn: Option<tidebreak_core::TurnId>,
    ) -> Result<fork::WrittenTranscript, ServerError> {
        let session = self.get_session(owner, session_id).await?;
        let workspace = self
            .require_live_workspace(owner, Self::require_workspace_id(&session)?)
            .await?;
        let private_root = crate::code::scratch::workspace_root(&self.data_dir, workspace.id)
            .map_err(|err| {
                ServerError::internal(format!("could not open private storage: {err}"))
            })?;

        // A session-level fork promises all accepted work through the newest
        // turn. Reserve the checkout long enough to take that snapshot so a
        // turn cannot start or finish halfway through preparation. An
        // explicit earlier turn is already a stable seam and stays available
        // while later work runs.
        let turn_lock = at_turn
            .is_none()
            .then(|| self.worktree_turn_lock(workspace.id));
        let _turn_guard = turn_lock
            .as_ref()
            .map(|lock| {
                lock.try_lock().map_err(|_| {
                    ServerError::conflict_kind(
                        "fork_turn_unsettled",
                        "a turn is still changing this workspace; wait for it to finish, or fork from an earlier completed turn",
                    )
                })
            })
            .transpose()?;

        let turns = list_turns(&self.db, owner, session_id).await?;
        let pending_approval_turns: HashSet<TurnId> = list_approvals(
            &self.db,
            owner,
            Some(ApprovalState::Pending),
            Some(session_id),
        )
        .await?
        .into_iter()
        .map(|approval| approval.turn_id)
        .collect();
        let has_queued_follow_up = at_turn.is_none()
            && queued_turn_head(&self.db, owner, session_id)
                .await?
                .is_some();
        let Some(prepared_cut) = fork::cut_at(&turns, at_turn) else {
            return Err(ServerError::bad_request(
                "that turn is not part of this session",
            ));
        };
        let Some(boundary) = prepared_cut.turns.last() else {
            let error = fork::ForkBoundaryError::NoTurns;
            return Err(ServerError::conflict_kind(error.kind(), error.message()));
        };
        let replay =
            list_fork_events(&self.db, owner, session_id, boundary.id, MAX_REPLAY_EVENTS).await?;
        let cut = fork::cut_at_settled_boundary(
            &turns,
            replay.boundary_status,
            &pending_approval_turns,
            has_queued_follow_up,
            at_turn,
        )
        .map_err(|error| match error {
            fork::ForkBoundaryError::UnknownTurn => ServerError::bad_request(error.message()),
            _ => ServerError::conflict_kind(error.kind(), error.message()),
        })?;
        drop(_turn_guard);

        fork::write_transcript(
            &private_root,
            self.blobs.as_ref(),
            &session,
            cut,
            &replay.events,
            &replay.complete_turns,
        )
        .await
        .map_err(|err| ServerError::internal(format!("could not write the fork transcript: {err}")))
    }
}
