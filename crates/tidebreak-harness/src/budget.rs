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
    pub fn push(&mut self, bytes: &[u8], budget: StreamBudget) -> BudgetTick {
        let mut overflow_chunks = 0;
        let incoming = String::from_utf8_lossy(bytes);
        if self.pending.len().saturating_add(incoming.len()) > budget.max_partial_line {
            overflow_chunks += 1;
            self.overflow_chunks += 1;
            let room = budget.max_partial_line.saturating_sub(self.pending.len());
            self.pending.push_str(&incoming[..incoming.len().min(room)]);
            // Drop the rest of this chunk but keep looking for a newline so a
            // later delimiter can flush the capped prefix.
            if let Some(idx) = incoming.find('\n') {
                let _ = idx;
            } else {
                return BudgetTick {
                    lines: Vec::new(),
                    overflow_chunks,
                };
            }
        } else {
            self.pending.push_str(&incoming);
        }

        let mut lines = Vec::new();
        while let Some(idx) = self.pending.find('\n') {
            let mut line = self.pending.drain(..=idx).collect::<String>();
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            lines.push(line);
        }
        if self.pending.len() > budget.max_partial_line {
            self.pending.truncate(budget.max_partial_line);
            overflow_chunks += 1;
            self.overflow_chunks += 1;
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
}
