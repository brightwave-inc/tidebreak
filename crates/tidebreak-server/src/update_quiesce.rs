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

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::code::CodeRuntime;

/// How long code turns may keep running before the quiesce gives up. Parking
/// idle sessions takes well under a second; this deadline exists only for
/// turns that were mid-flight when the restart was requested.
const CODE_TURN_DEADLINE: Duration = Duration::from_secs(20);

/// Upper bound on the chat drain, past the worker's own finish grace. The
/// drain cannot refuse — claims that outlive the grace are aborted and their
/// leases handed back — so this only guards against a wedged worker task.
const CHAT_DRAIN_DEADLINE: Duration = Duration::from_secs(15);

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

/// Brings the process's live work to a restart-safe point, and back.
///
/// Handed to native embedders so a restart-to-update can park code engine
/// children at a turn boundary and release chat turn leases before the
/// bundle is replaced. Cloneable; all clones drive the same process state.
#[derive(Clone)]
pub struct UpdateQuiesce {
    code: Arc<CodeRuntime>,
    chat: ChatQuiesceControl,
}

impl UpdateQuiesce {
    pub(crate) fn new(code: Arc<CodeRuntime>, chat: UpdateQuiesceChatHalf) -> Self {
        Self { code, chat: chat.0 }
    }

    /// Stop new turns, park code engine children at their next turn boundary,
    /// and hand back chat turn leases.
    ///
    /// On success the process holds no engine child and no live chat lease,
    /// so exiting loses nothing recovery cannot resume. On failure — a code
    /// turn still running at the deadline — everything is resumed and the
    /// error is a sentence the updater can show as-is.
    pub async fn quiesce_for_update(&self) -> Result<(), String> {
        self.chat.request.send_replace(true);
        self.code.begin_update_quiesce();
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

    /// Reopen turn admission after an update that did not install.
    ///
    /// Parked engine children stay parked — the next turn respawns and
    /// resumes them, exactly as an idle park does — so there is nothing to
    /// relaunch eagerly here.
    pub fn resume_after_failed_update(&self) {
        self.chat.request.send_replace(false);
        self.code.end_update_quiesce();
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
