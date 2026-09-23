//! Bounded stream-parse budgets.
//!
//! Parsing is O(new bytes). Overflow is counted, never silently dropped: a
//! line longer than the cap arrives marked as cut, so the parser can recover
//! what the line started with instead of discarding the whole event.

use std::borrow::Cow;

/// Default chunk size for one read from an engine pipe.
pub const DEFAULT_CHUNK_SIZE: usize = 8_192;
/// Default number of chunks processed before yielding.
pub const DEFAULT_CHUNKS_PER_TICK: usize = 8;
/// Hard cap on a partial line held across reads.
pub const DEFAULT_MAX_PARTIAL_LINE: usize = 256 * 1_024;

/// Fixed limits for one engine stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamBudget {
    /// Bytes requested per `read`.
    pub chunk_size: usize,
    /// Chunks processed before the reader must yield.
    pub max_chunks_per_tick: usize,
    /// Hard cap on a buffered partial line.
    pub max_partial_line: usize,
}

impl Default for StreamBudget {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_chunks_per_tick: DEFAULT_CHUNKS_PER_TICK,
            max_partial_line: DEFAULT_MAX_PARTIAL_LINE,
        }
    }
}

/// One complete line taken from the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLine {
    /// The line without its newline. A cut line holds only the bytes before
    /// the cap.
    pub text: String,
    /// Whether the line outgrew the cap and lost everything after it.
    pub cut: bool,
}

impl StreamLine {
    /// A line that arrived whole.
    #[must_use]
    pub fn whole(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cut: false,
        }
    }
}

/// Outcome of pushing one chunk into the line buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetTick {
    /// Complete lines extracted this tick, already stripped of `\n`.
    pub lines: Vec<StreamLine>,
    /// Number of overflow chunks this tick (bytes that did not fit).
    pub overflow_chunks: u64,
}

/// Partial-line buffer with a hard cap. Overflow is counted.
#[derive(Debug, Default)]
pub struct StreamLineBuffer {
    pending: Vec<u8>,
    /// Whether the line currently being buffered has already hit the cap, so
    /// the rest of it is dropped until its newline arrives.
    overflowing: bool,
    /// Total overflow chunks observed over the life of the buffer.
    pub overflow_chunks: u64,
}

impl StreamLineBuffer {
    /// Empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push `bytes` and take complete lines, decoding only at line boundaries.
    ///
    /// A line longer than `budget.max_partial_line` is emitted cut at its cap
    /// once its newline arrives, marked [`StreamLine::cut`], and the bytes
    /// beyond the cap are counted as overflow. The stream keeps flowing either
    /// way: one oversized line must never stop later lines from being
    /// delivered.
    pub fn push(&mut self, bytes: &[u8], budget: StreamBudget) -> BudgetTick {
        let mut rest = bytes;
        let mut lines = Vec::new();
        let mut overflow_chunks = 0;

        while !rest.is_empty() {
            let (segment, tail) = match rest.iter().position(|byte| *byte == b'\n') {
                Some(idx) => (&rest[..idx], Some(&rest[idx + 1..])),
                None => (rest, None),
            };

            if self.overflowing {
                if !segment.is_empty() {
                    overflow_chunks += 1;
                    self.overflow_chunks += 1;
                }
            } else {
                let room = budget.max_partial_line.saturating_sub(self.pending.len());
                if segment.len() > room {
                    self.pending.extend_from_slice(&segment[..room]);
                    if let Err(error) = std::str::from_utf8(&self.pending) {
                        if error.error_len().is_none() {
                            self.pending.truncate(error.valid_up_to());
                        }
                    }
                    self.overflowing = true;
                    overflow_chunks += 1;
                    self.overflow_chunks += 1;
                } else {
                    self.pending.extend_from_slice(segment);
                }
            }

            match tail {
                Some(tail) => {
                    let mut line = std::mem::take(&mut self.pending);
                    if line.ends_with(b"\r") {
                        line.pop();
                    }
                    lines.push(StreamLine {
                        text: String::from_utf8_lossy(&line).into_owned(),
                        cut: self.overflowing,
                    });
                    self.overflowing = false;
                    rest = tail;
                }
                None => rest = b"",
            }
        }

        BudgetTick {
            lines,
            overflow_chunks,
        }
    }

    /// Remaining partial line, if any.
    #[must_use]
    pub fn pending(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.pending)
    }

    /// The remaining partial line as a line the stream ended on, marked cut
    /// when it had already outgrown the cap. `None` when nothing is pending.
    #[must_use]
    pub fn pending_line(&self) -> Option<StreamLine> {
        (!self.pending.is_empty()).then(|| StreamLine {
            text: self.pending().into_owned(),
            cut: self.overflowing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(tick: &BudgetTick) -> Vec<&str> {
        tick.lines.iter().map(|line| line.text.as_str()).collect()
    }

    #[test]
    fn splits_complete_lines_and_keeps_partial() {
        let mut buf = StreamLineBuffer::new();
        let tick = buf.push(b"one\ntwo\nthr", StreamBudget::default());
        assert_eq!(texts(&tick), ["one", "two"]);
        assert!(tick.lines.iter().all(|line| !line.cut));
        assert_eq!(buf.pending(), "thr");
        let tick = buf.push(b"ee\n", StreamBudget::default());
        assert_eq!(texts(&tick), ["three"]);
        assert!(buf.pending().is_empty());
        assert_eq!(buf.pending_line(), None);
    }

    #[test]
    fn decodes_a_character_split_across_chunks_at_the_line_boundary() {
        let mut buf = StreamLineBuffer::new();
        let emoji = "🙂".as_bytes();
        assert!(buf
            .push(&emoji[..2], StreamBudget::default())
            .lines
            .is_empty());
        let tick = buf.push(&[emoji[2], emoji[3], b'\n'], StreamBudget::default());
        assert_eq!(texts(&tick), ["🙂"]);
    }

    #[test]
    fn overflow_is_counted_not_dropped_silently() {
        let budget = StreamBudget {
            chunk_size: 8,
            max_chunks_per_tick: 1,
            max_partial_line: 8,
        };
        let mut buf = StreamLineBuffer::new();
        let tick = buf.push(b"abcdefghijklmnop", budget);
        assert!(tick.overflow_chunks >= 1);
        assert_eq!(buf.overflow_chunks, tick.overflow_chunks);
        assert!(buf.pending().len() <= 8);
        // A stream that ends on the oversized line still says it was cut.
        assert_eq!(
            buf.pending_line(),
            Some(StreamLine {
                text: "abcdefgh".into(),
                cut: true
            })
        );
    }

    #[test]
    fn an_oversized_line_does_not_wedge_the_stream() {
        let budget = StreamBudget {
            chunk_size: 8,
            max_chunks_per_tick: 1,
            max_partial_line: 8,
        };
        let mut buf = StreamLineBuffer::new();

        // An oversized line arriving in pieces, then a normal line behind it.
        assert!(buf.push(b"aaaaaaaaaabbbb", budget).lines.is_empty());
        assert!(buf.push(b"cccccccccc", budget).lines.is_empty());
        let tick = buf.push(b"dddd\nnormal\n", budget);
        assert_eq!(
            tick.lines,
            [
                StreamLine {
                    text: "aaaaaaaa".into(),
                    cut: true
                },
                StreamLine::whole("normal"),
            ]
        );
        assert!(buf.overflow_chunks >= 1);
        assert!(buf.pending().is_empty());

        // And the buffer keeps working afterwards.
        let tick = buf.push(b"after\n", budget);
        assert_eq!(tick.lines, [StreamLine::whole("after")]);
    }

    #[test]
    fn a_line_exactly_at_the_cap_is_not_cut() {
        let budget = StreamBudget {
            chunk_size: 8,
            max_chunks_per_tick: 1,
            max_partial_line: 8,
        };
        let mut buf = StreamLineBuffer::new();
        let tick = buf.push(b"abcdefgh\n", budget);
        assert_eq!(tick.lines, [StreamLine::whole("abcdefgh")]);
        assert_eq!(tick.overflow_chunks, 0);
    }

    #[test]
    fn a_character_straddling_the_cap_is_dropped_not_split() {
        let budget = StreamBudget {
            chunk_size: 8,
            max_chunks_per_tick: 1,
            // "é" is two bytes, so a cap of 5 lands inside the third one.
            max_partial_line: 5,
        };
        let mut buf = StreamLineBuffer::new();
        let tick = buf.push("ééé\nplain\n".as_bytes(), budget);
        assert_eq!(texts(&tick), ["éé", "plain"]);
        assert!(tick.lines[0].cut);
    }
}
