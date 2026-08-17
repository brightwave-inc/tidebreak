//! Bounded stream-parse budgets.
//!
//! Parsing is O(new bytes). Overflow is counted, never silently dropped.

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

/// Outcome of pushing one chunk into the line buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetTick {
    /// Complete lines extracted this tick, already stripped of `\n`.
    pub lines: Vec<String>,
    /// Number of overflow chunks this tick (bytes that did not fit).
    pub overflow_chunks: u64,
}

/// Partial-line buffer with a hard cap. Overflow is counted.
#[derive(Debug, Default)]
pub struct StreamLineBuffer {
    pending: String,
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

    /// Push `bytes` (lossy UTF-8) and take any complete lines.
    ///
    /// A line longer than `budget.max_partial_line` is emitted truncated at
    /// its cap once its newline arrives, and the bytes beyond the cap are
    /// counted as overflow. The stream keeps flowing either way: one oversized
    /// line must never stop later lines from being delivered.
    pub fn push(&mut self, bytes: &[u8], budget: StreamBudget) -> BudgetTick {
        let incoming = String::from_utf8_lossy(bytes);
        let mut rest: &str = incoming.as_ref();
        let mut lines = Vec::new();
        let mut overflow_chunks = 0;

        while !rest.is_empty() {
            let (segment, tail) = match rest.find('\n') {
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
                    let end = crate::text::floor_char_boundary(segment, room);
                    self.pending.push_str(&segment[..end]);
                    self.overflowing = true;
                    overflow_chunks += 1;
                    self.overflow_chunks += 1;
                } else {
                    self.pending.push_str(segment);
                }
            }

            match tail {
                Some(tail) => {
                    let mut line = std::mem::take(&mut self.pending);
                    if line.ends_with('\r') {
                        line.pop();
                    }
                    lines.push(line);
                    self.overflowing = false;
                    rest = tail;
                }
                None => rest = "",
            }
        }

        BudgetTick {
            lines,
            overflow_chunks,
        }
    }

    /// Remaining partial line, if any.
    #[must_use]
    pub fn pending(&self) -> &str {
        &self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_complete_lines_and_keeps_partial() {
        let mut buf = StreamLineBuffer::new();
        let tick = buf.push(b"one\ntwo\nthr", StreamBudget::default());
        assert_eq!(tick.lines, ["one", "two"]);
        assert_eq!(buf.pending(), "thr");
        let tick = buf.push(b"ee\n", StreamBudget::default());
        assert_eq!(tick.lines, ["three"]);
        assert!(buf.pending().is_empty());
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
        assert_eq!(tick.lines, ["aaaaaaaa", "normal"]);
        assert!(buf.overflow_chunks >= 1);
        assert!(buf.pending().is_empty());

        // And the buffer keeps working afterwards.
        let tick = buf.push(b"after\n", budget);
        assert_eq!(tick.lines, ["after"]);
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
        assert_eq!(tick.lines, ["éé", "plain"]);
    }
}
