//! Quiesce live work before a restart-to-update.
//!
//! Restarting the process used to orphan every code engine child (boot
//! recovery then fenced their sessions) and abandon chat turn leases their
//! successor had to wait out. The embedder asks this handle to bring the
//! process to a safe point first:
//!
//! - Code sessions park at a turn boundary. New turns stop starting (sends
//!   queue durably instead), in-flight turns run to completion, and each
//!   idle engine child is released through the same park-and-resume path
//!   decision 0064 already proves. After the relaunch every session is
//!   `Idle` with a stored resume ref and a queue that drains on its own.
//! - Chat turns get a short grace to finish, then the remaining claims are
//!   aborted and their durable leases handed back, so the relaunched worker
//!   re-claims them immediately instead of waiting out the lease. An abort
//!   is the crash the lease protocol already recovers from: the retry
//!   rebuilds the transcript and re-runs only the model call in flight.
//!
//! No engine supports resuming mid-turn, so a code turn that outruns the
//! deadline fails the quiesce rather than being interrupted: the update
//! stays staged and the caller retries once the turn finishes.
//!
//! Quitting the desktop app reuses the same handle.
//! [`UpdateQuiesce::working_agents`] tells the app whether to ask first. A
//! quit that waits for a safe point runs the same quiesce with no deadline,
//! and a quit that stops the agents cancels chat turns and interrupts code
//! turns before parking.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::Utc;
use tidebreak_core::{SessionId, Store, TurnId};
use tokio::sync::watch;

use crate::bus::EventBus;
use crate::code::{CodeRuntime, SessionSafePoint};
use crate::state::TurnGuard;

/// How long code turns may keep running before the quiesce gives up. Parking
/// idle sessions takes well under a second; this deadline exists only for
/// turns that were mid-flight when the restart was requested.
const CODE_TURN_DEADLINE: Duration = Duration::from_secs(20);

/// Upper bound on the chat drain, past the worker's own finish grace. The
/// drain cannot refuse — claims that outlive the grace are aborted and their
/// leases handed back — so this only guards against a wedged worker task.
const CHAT_DRAIN_DEADLINE: Duration = Duration::from_secs(15);

/// How long a quit that stops the agents waits for interrupted code turns to
/// reach their boundary and for engine children to park. The app quits when
/// this passes either way.
const STOP_DEADLINE: Duration = Duration::from_secs(15);

/// How many times a quit retries a chat turn's cancellation that a heartbeat
/// raced. Each retry reads fresh time, so one is nearly always enough.
const CANCEL_ATTEMPTS: usize = 20;

/// The chat turn worker's half: it watches `request` and reports on `drained`.
pub(crate) struct ChatQuiesceWorker {
    pub(crate) request: watch::Receiver<bool>,
    pub(crate) drained: watch::Sender<bool>,
}

impl Clone for ChatQuiesceWorker {
    fn clone(&self) -> Self {
        Self {
            request: self.request.clone(),
            drained: self.drained.clone(),
        }
    }
}

/// The controller's half, held by [`UpdateQuiesce`].
#[derive(Clone)]
struct ChatQuiesceControl {
    request: watch::Sender<bool>,
    drained: watch::Receiver<bool>,
}

pub(crate) fn chat_quiesce_pair() -> (ChatQuiesceWorker, UpdateQuiesceChatHalf) {
    let (request_tx, request_rx) = watch::channel(false);
    let (drained_tx, drained_rx) = watch::channel(false);
    (
        ChatQuiesceWorker {
            request: request_rx,
            drained: drained_tx,
        },
        UpdateQuiesceChatHalf(ChatQuiesceControl {
            request: request_tx,
            drained: drained_rx,
        }),
    )
}

/// Opaque chat half handed to [`UpdateQuiesce::new`] by the binder.
#[derive(Clone)]
pub(crate) struct UpdateQuiesceChatHalf(ChatQuiesceControl);

/// Why the process is held at a safe point. An update and a quit can overlap:
/// a quit can start while an update is quiescing, and an update can start
/// while a quit waits. Each releases only its own hold, and turn admission
/// reopens once neither holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    Update,
    Quit,
}

#[derive(Default)]
struct Holds {
    update: bool,
    quit: bool,
}

impl Holds {
    fn set(&mut self, hold: Hold, held: bool) {
        match hold {
            Hold::Update => self.update = held,
            Hold::Quit => self.quit = held,
        }
    }

    fn any(&self) -> bool {
        self.update || self.quit
    }
}

/// Agents that keep a quit from being immediate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuitProgress {
    /// Agents not at a safe point, each session counted once.
    pub agents: usize,
    /// Of those, the ones parked on the person: an approval, a question, or a
    /// plan waits for an answer, so they cannot reach a safe point alone.
    pub waiting_for_you: usize,
}

/// Brings the process's live work to a restart-safe point, and back.
///
/// Handed to native embedders so a restart-to-update can park code engine
/// children at a turn boundary and release chat turn leases before the
/// bundle is replaced, and so a quit can see and stop the work in flight.
/// Cloneable; all clones drive the same process state.
#[derive(Clone)]
pub struct UpdateQuiesce {
    code: Arc<CodeRuntime>,
    chat: ChatQuiesceControl,
    /// The chat turns executing in this process, for counting and stopping.
    turns: Arc<TurnGuard>,
    store: Arc<dyn Store>,
    events: Arc<EventBus>,
    holds: Arc<Mutex<Holds>>,
}

impl UpdateQuiesce {
    pub(crate) fn new(
        code: Arc<CodeRuntime>,
        chat: UpdateQuiesceChatHalf,
        turns: Arc<TurnGuard>,
        store: Arc<dyn Store>,
        events: Arc<EventBus>,
    ) -> Self {
        Self {
            code,
            chat: chat.0,
            turns,
            store,
            events,
            holds: Arc::new(Mutex::new(Holds::default())),
        }
    }

    /// Raise the quiesce flags on behalf of `hold`.
    fn hold(&self, hold: Hold) {
        let mut holds = self.holds.lock().unwrap_or_else(PoisonError::into_inner);
        holds.set(hold, true);
        self.chat.request.send_replace(true);
        self.code.begin_update_quiesce();
    }

    /// Drop `hold`, and lower the flags only when nothing else holds them.
    fn release(&self, hold: Hold) {
        let mut holds = self.holds.lock().unwrap_or_else(PoisonError::into_inner);
        holds.set(hold, false);
        if !holds.any() {
            self.chat.request.send_replace(false);
            self.code.end_update_quiesce();
        }
    }

    /// The agents mid-turn in this process, for deciding whether a quit asks
    /// first: chat turns and local code turns, each session counted once. A
    /// code session on the internal engine runs its turn on the chat lane, so
    /// it shows up in both places. An idle session whose engine child has not
    /// parked yet is not working, so it does not count here.
    pub async fn working_agents(&self) -> Result<QuitProgress, String> {
        let code = self.code.sessions_short_of_a_safe_point().await?;
        Ok(self.progress(code.iter().filter(|session| session.mid_turn)))
    }

    /// The agents a quit that waits for a safe point is still waiting on:
    /// chat turns not yet released, and every local session the quiesce waits
    /// on, parking engine children included. This is exactly what
    /// [`Self::quiesce_for_quit`] waits for, so the count reaches zero when
    /// the wait ends.
    pub async fn safe_point_progress(&self) -> Result<QuitProgress, String> {
        let code = self.code.sessions_short_of_a_safe_point().await?;
        Ok(self.progress(code.iter()))
    }

    fn progress<'a>(&self, code: impl Iterator<Item = &'a SessionSafePoint>) -> QuitProgress {
        let mut agents: HashSet<SessionId> = self
            .turns
            .active()
            .into_iter()
            .map(|(chat_id, _)| chat_id)
            .collect();
        let mut waiting_for_you = 0;
        for session in code {
            agents.insert(session.id);
            if session.waiting_for_answer {
                waiting_for_you += 1;
            }
        }
        QuitProgress {
            agents: agents.len(),
            waiting_for_you,
        }
    }

    /// Park everything at a safe point before a quit, however long the turns
    /// in flight take.
    ///
    /// The same quiesce as an update, without the code deadline: new turns
    /// stop starting, code turns run to their boundary, idle engine children
    /// park, and chat turns hand their leases back so the next launch resumes
    /// them. A turn parked on an approval waits for the person's answer. To
    /// cancel, drop the future and call [`Self::resume_after_cancelled_quit`].
    pub async fn quiesce_for_quit(&self) -> Result<(), String> {
        self.hold(Hold::Quit);
        let (code, chat) = tokio::join!(
            self.code.await_turn_boundaries(None),
            Self::await_chat_drained(self.chat.drained.clone()),
        );
        if let Err(error) = code.and(chat) {
            self.release(Hold::Quit);
            return Err(error);
        }
        Ok(())
    }

    /// Stop every turn in flight before a quit.
    ///
    /// Chat turns are cancelled the way the Stop button cancels them, so the
    /// next launch does not resume them. Code turns on an external engine are
    /// interrupted. Then the quiesce parks what is left, for at most
    /// [`STOP_DEADLINE`]. An error means something had not parked by then;
    /// the caller quits anyway, so the hold is never released.
    pub async fn stop_for_quit(&self) -> Result<(), String> {
        self.hold(Hold::Quit);
        let chats = self.turns.active();
        let cancelled: HashSet<SessionId> = chats.iter().map(|(chat_id, _)| *chat_id).collect();
        for (chat_id, turn_id) in chats {
            self.cancel_chat_turn(chat_id, turn_id).await;
        }
        let code = self.code.sessions_short_of_a_safe_point().await?;
        for session in code.iter().filter(|session| session.mid_turn) {
            // A session on the internal engine ran its turn on the chat lane,
            // and cancelling that turn above already stopped it.
            if cancelled.contains(&session.id) {
                continue;
            }
            if let Err(error) = self.code.interrupt(session.id).await {
                tracing::warn!(
                    session = %session.id,
                    "could not interrupt a code turn before quitting: {}",
                    error.message()
                );
            }
        }
        let (code, chat) = tokio::join!(
            self.code.await_turn_boundaries(Some(STOP_DEADLINE)),
            Self::await_chat_drained(self.chat.drained.clone()),
        );
        code.and(chat)
    }

    /// Release a quit's hold after the person cancelled a quit that was
    /// waiting for a safe point. Turn admission reopens unless an update is
    /// quiescing too, whose hold stays in force.
    pub fn resume_after_cancelled_quit(&self) {
        self.release(Hold::Quit);
    }

    /// Record a durable cancellation for one local chat turn and trip its
    /// local signal, the way `POST /chats/{id}/cancel` does.
    async fn cancel_chat_turn(&self, chat_id: SessionId, turn_id: TurnId) {
        for _ in 0..CANCEL_ATTEMPTS {
            match self
                .store
                .request_turn_cancellation_and_append_event(turn_id, Utc::now())
                .await
            {
                Ok(Some(resolution)) => {
                    if let Some(event) = resolution.terminal_event {
                        let _ = self.events.sender(chat_id).send(event);
                    }
                    break;
                }
                // A heartbeat moved the row under this request; retry with
                // fresh time.
                Ok(None) => tokio::task::yield_now().await,
                Err(error) => {
                    tracing::warn!(
                        chat = %chat_id,
                        "could not cancel a chat turn before quitting: {error}"
                    );
                    break;
                }
            }
        }
        self.turns.cancel(chat_id, turn_id);
    }

    /// Stop new turns, park code engine children at their next turn boundary,
    /// and hand back chat turn leases.
    ///
    /// On success the process holds no engine child and no live chat lease,
    /// so exiting loses nothing recovery cannot resume. On failure — a code
    /// turn still running at the deadline — the update's hold is released and
    /// the error is a sentence the updater can show as-is.
    pub async fn quiesce_for_update(&self) -> Result<(), String> {
        self.hold(Hold::Update);
        let (code, chat) = tokio::join!(
            self.code.await_update_quiesce(CODE_TURN_DEADLINE),
            Self::await_chat_drained(self.chat.drained.clone()),
        );
        if let Err(error) = code.and(chat) {
            self.resume_after_failed_update();
            return Err(error);
        }
        Ok(())
    }

    /// Release the update's hold after an update that did not install. Turn
    /// admission reopens unless a quit is waiting for a safe point too.
    ///
    /// Parked engine children stay parked — the next turn respawns and
    /// resumes them, exactly as an idle park does — so there is nothing to
    /// relaunch eagerly here.
    pub fn resume_after_failed_update(&self) {
        self.release(Hold::Update);
    }

    async fn await_chat_drained(mut drained: watch::Receiver<bool>) -> Result<(), String> {
        let wait = async {
            loop {
                if *drained.borrow_and_update() {
                    return Ok(());
                }
                if drained.changed().await.is_err() {
                    return Err("the chat turn worker exited before it could drain".to_owned());
                }
            }
        };
        match tokio::time::timeout(CHAT_DRAIN_DEADLINE, wait).await {
            Ok(result) => result,
            Err(_) => {
                Err("Chat turns could not be released in time. Try again in a moment.".to_owned())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tidebreak_core::{Chat, DbStore, TurnRunStatus};
    use uuid::Uuid;

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        quiesce: UpdateQuiesce,
        turns: Arc<TurnGuard>,
        db: Arc<DbStore>,
        worker: ChatQuiesceWorker,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("quiesce.db").display()
            ))
            .await
            .unwrap(),
        );
        let code = Arc::new(CodeRuntime::new(
            db.clone(),
            dir.path().into(),
            None,
            None,
            None,
            None,
            None,
            None,
        ));
        let (worker, control) = chat_quiesce_pair();
        let turns = Arc::new(TurnGuard::default());
        let quiesce = UpdateQuiesce::new(
            code,
            control,
            turns.clone(),
            db.clone(),
            Arc::new(EventBus::default()),
        );
        Fixture {
            _dir: dir,
            quiesce,
            turns,
            db,
            worker,
        }
    }

    /// Stands in for the chat turn worker: reports drained whenever a quiesce
    /// asks, the way the worker does once its turns are released.
    fn drain_on_request(worker: ChatQuiesceWorker) {
        tokio::spawn(async move {
            let ChatQuiesceWorker {
                mut request,
                drained,
            } = worker;
            loop {
                if *request.borrow_and_update() {
                    let _ = drained.send(true);
                }
                if request.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    async fn running_chat_turn(db: &DbStore) -> (SessionId, TurnId, Uuid) {
        let chat = Chat {
            id: SessionId::new(),
            project_id: None,
            title: Some("quit".into()),
            model: Some("model".into()),
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        };
        db.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        db.accept_turn(turn_id, chat.id, "model", "keep working")
            .await
            .unwrap();
        let lease = Uuid::new_v4();
        let now = Utc::now();
        let claimed = db
            .claim_turn(lease, now, now + chrono::Duration::hours(1))
            .await
            .unwrap()
            .turn
            .expect("the accepted turn is claimable");
        assert_eq!(claimed.id, turn_id);
        (chat.id, turn_id, lease)
    }

    /// The quit prompt names how many agents are working. Each local chat
    /// turn counts once, and a finished turn stops counting.
    #[tokio::test]
    async fn working_agents_counts_each_running_turn_once() {
        let fixture = fixture().await;
        let agents = |progress: QuitProgress| progress.agents;
        assert_eq!(agents(fixture.quiesce.working_agents().await.unwrap()), 0);

        let first = fixture
            .turns
            .register(SessionId::new(), TurnId::new(), Uuid::new_v4())
            .unwrap();
        let second = fixture
            .turns
            .register(SessionId::new(), TurnId::new(), Uuid::new_v4())
            .unwrap();
        assert_eq!(agents(fixture.quiesce.working_agents().await.unwrap()), 2);

        drop(first);
        assert_eq!(agents(fixture.quiesce.working_agents().await.unwrap()), 1);
        drop(second);
        assert_eq!(agents(fixture.quiesce.working_agents().await.unwrap()), 0);
    }

    /// The first question counts only agents mid-turn; once the quit waits,
    /// it counts what the wait waits on, a parking engine child included, so
    /// the prompt never reads zero while the wait goes on. A turn parked on
    /// an approval counts in both, and says it waits on the person. A code
    /// session on the internal engine is counted once, whichever lane it
    /// shows up in.
    #[tokio::test]
    async fn the_quit_counts_agree_with_what_the_wait_waits_on() {
        let fixture = fixture().await;
        let shared = SessionId::new();
        let _chat_turn = fixture
            .turns
            .register(shared, TurnId::new(), Uuid::new_v4())
            .unwrap();
        let code = [
            SessionSafePoint {
                id: shared,
                mid_turn: true,
                waiting_for_answer: false,
            },
            SessionSafePoint {
                id: SessionId::new(),
                mid_turn: true,
                waiting_for_answer: true,
            },
            SessionSafePoint {
                id: SessionId::new(),
                mid_turn: false,
                waiting_for_answer: false,
            },
        ];

        let asking = fixture
            .quiesce
            .progress(code.iter().filter(|session| session.mid_turn));
        assert_eq!(
            asking,
            QuitProgress {
                agents: 2,
                waiting_for_you: 1,
            }
        );
        let waiting = fixture.quiesce.progress(code.iter());
        assert_eq!(
            waiting,
            QuitProgress {
                agents: 3,
                waiting_for_you: 1,
            }
        );
    }

    /// "Quit and stop them" cancels a running chat turn the way the Stop
    /// button does: the worker's signal trips, and the row is durably
    /// cancelling, so the next launch finishes the cancellation instead of
    /// resuming the turn.
    #[tokio::test]
    async fn stopping_for_a_quit_cancels_chat_turns_durably() {
        let fixture = fixture().await;
        drain_on_request(fixture.worker.clone());
        let (chat_id, turn_id, lease) = running_chat_turn(&fixture.db).await;
        let active = fixture.turns.register(chat_id, turn_id, lease).unwrap();

        fixture.quiesce.stop_for_quit().await.unwrap();

        assert!(active.cancel_token().is_cancelled());
        let turn = fixture.db.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.status, TurnRunStatus::Cancelling);
    }

    /// Waiting for a safe point closes turn admission until the person cancels
    /// the quit, and a cancelled quit reopens it.
    #[tokio::test]
    async fn a_cancelled_quit_reopens_turn_admission() {
        let fixture = fixture().await;
        drain_on_request(fixture.worker.clone());

        fixture.quiesce.quiesce_for_quit().await.unwrap();
        assert!(*fixture.worker.request.borrow());
        assert!(fixture.quiesce.code.update_quiesce_active());

        fixture.quiesce.resume_after_cancelled_quit();
        assert!(!*fixture.worker.request.borrow());
        assert!(!fixture.quiesce.code.update_quiesce_active());
    }

    /// Click Restart to update, press Cmd+Q, choose the safe point, then
    /// Cancel: the update's quiesce stays in force until the update itself
    /// lets go. The reverse holds too: a failed update does not reopen
    /// admission under a quit that is still waiting.
    #[tokio::test]
    async fn a_quit_and_an_update_each_release_only_their_own_hold() {
        let fixture = fixture().await;
        drain_on_request(fixture.worker.clone());
        let held = |fixture: &Fixture| {
            let chat = *fixture.worker.request.borrow();
            let code = fixture.quiesce.code.update_quiesce_active();
            assert_eq!(chat, code, "the chat and code flags move together");
            chat
        };

        fixture.quiesce.quiesce_for_update().await.unwrap();
        fixture.quiesce.quiesce_for_quit().await.unwrap();
        fixture.quiesce.resume_after_cancelled_quit();
        assert!(held(&fixture), "the update still holds the quiesce");
        fixture.quiesce.resume_after_failed_update();
        assert!(!held(&fixture));

        fixture.quiesce.quiesce_for_quit().await.unwrap();
        fixture.quiesce.quiesce_for_update().await.unwrap();
        fixture.quiesce.resume_after_failed_update();
        assert!(held(&fixture), "the waiting quit still holds the quiesce");
        fixture.quiesce.resume_after_cancelled_quit();
        assert!(!held(&fixture));
    }
}
