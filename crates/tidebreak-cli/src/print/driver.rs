//! The stdin side of the driving protocol.
//!
//! A driver is attached when the caller asked for NDJSON output *and* stdin is
//! something other than a terminal — a pipe, a socket, a file. Reading a
//! decision from an unattached driver, or from one whose input has ended,
//! yields nothing, and the caller falls back to the standing policy in
//! [`super::protocol::Interaction::undriven`]. That is what keeps
//! `tidebreak -p … --output-format json < /dev/null` from blocking forever on
//! an answer nobody is going to write.

use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, BufReader, Lines, Stdin};

use super::protocol::{error_event, parse_decision, Decision, Interaction};

/// The decision channel: either a reader of NDJSON decision lines, or nothing.
pub struct Driver<R> {
    lines: Option<Lines<R>>,
}

impl Driver<BufReader<Stdin>> {
    /// Attach to stdin when the invocation opted into driving, otherwise not.
    pub fn from_stdin(driving: bool) -> Self {
        Self {
            lines: driving.then(|| BufReader::new(tokio::io::stdin()).lines()),
        }
    }
}

impl<R: AsyncBufRead + Unpin> Driver<R> {
    /// A driver reading decisions from `reader`. Production attaches to stdin;
    /// this is how the protocol is exercised over an in-memory channel.
    #[cfg(test)]
    pub fn attached(reader: R) -> Self {
        Self {
            lines: Some(reader.lines()),
        }
    }

    /// Ask the driver to settle `interaction`.
    ///
    /// Emits the request event, then reads lines until one decides it.
    /// `None` means no driver answered — unattached, or the input ended — and
    /// the standing policy applies. Lines that cannot decide the pending
    /// request produce an `error` event and are skipped, so one malformed line
    /// does not lose the session.
    pub async fn decide(
        &mut self,
        interaction: &Interaction,
        emit: &mut dyn FnMut(serde_json::Value),
    ) -> Option<Decision> {
        let lines = self.lines.as_mut()?;
        emit(interaction.request_event());
        loop {
            // An I/O error on stdin is the end of the channel, same as EOF:
            // there is no answer coming either way.
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) | Err(_) => {
                    // Stop reading: every later interaction takes the standing
                    // policy rather than re-reading a closed channel.
                    self.lines = None;
                    return None;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match parse_decision(&line, interaction) {
                Ok(decision) => return Some(decision),
                Err(message) => emit(error_event(&message)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tidebreak_core::CallId;

    use super::*;
    use crate::print::protocol::{HaltReason, Undriven};

    fn plan_interaction(call_id: CallId) -> Interaction {
        Interaction::Plan {
            call_id,
            title: "Rewrite the importer".into(),
            plan: "1. read\n2. write".into(),
        }
    }

    /// The driven path, end to end at the protocol seam: the proposal is
    /// emitted, a decision line answers it, and the decision the run carries
    /// out is the one the *line* asked for.
    ///
    /// Rejecting with feedback is deliberate: a build that answered plans from
    /// policy could only accept or halt, so neither could produce this.
    #[tokio::test]
    async fn a_driven_plan_proposal_is_settled_by_the_stdin_line() {
        let call_id = CallId::new();
        let interaction = plan_interaction(call_id);
        let input = format!(
            "{}\n{}\n",
            r#"{"type":"questions","answers":[]}"#,
            format_args!(
                r#"{{"type":"plan","call_id":"{call_id}","decision":"reject","feedback":"split it in two"}}"#
            ),
        );
        let mut driver = Driver::attached(input.as_bytes());

        let mut emitted = Vec::new();
        let decision = driver
            .decide(&interaction, &mut |event| emitted.push(event))
            .await;

        assert_eq!(
            decision,
            Some(Decision::Plan {
                accept: false,
                feedback: Some("split it in two".into()),
                permission_mode: None,
            })
        );
        // The proposal reached the driver with its content, and the line that
        // could not answer it was reported rather than applied.
        assert_eq!(emitted[0]["type"], "plan_proposal");
        assert_eq!(emitted[0]["tidebreak"], "v1");
        assert_eq!(emitted[0]["title"], "Rewrite the importer");
        assert_eq!(emitted[1]["type"], "error");
        assert_eq!(emitted.len(), 2, "{emitted:?}");
    }

    /// With no driver — and with one whose input ends without answering — a
    /// question halts the run with the documented, machine-readable reason
    /// rather than cancelling silently.
    #[tokio::test]
    async fn an_unanswered_question_halts_with_its_reason() {
        let call_id = CallId::new();
        let interaction = Interaction::Questions {
            call_id,
            questions: Vec::new(),
        };

        for mut driver in [
            Driver::attached(&b""[..]),
            Driver {
                lines: None::<Lines<&[u8]>>,
            },
        ] {
            let mut emitted = Vec::new();
            let decision = driver
                .decide(&interaction, &mut |event| emitted.push(event))
                .await;
            assert_eq!(decision, None);

            let Undriven::Halt(halt) = interaction.undriven() else {
                panic!("an unanswered question must halt");
            };
            assert_eq!(halt.reason, HaltReason::QuestionsUndriven);
            let event = halt.event();
            assert_eq!(event["type"], "halted");
            assert_eq!(event["reason"], "questions_undriven");
            assert_eq!(event["exit_code"], 3);
            assert_eq!(event["call_id"], serde_json::json!(call_id));
        }
    }
}
