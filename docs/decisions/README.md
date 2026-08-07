# Decision records

Numbered records of decisions that later work has to live with. Each states
what was chosen, what was rejected, and what would cause it to be revisited.

The files in this directory are the index — they sort by number and their names
say what they decide. Nothing lists them elsewhere, so nothing goes stale.

To add one, copy [`0000-template.md`](0000-template.md), take the next number,
and open it as a PR. Merging is the acceptance; a record still under discussion
says `Proposed`. When a decision changes, the old record gets
`Status: Superseded` and a pointer to its replacement rather than being
rewritten — the value is the reasoning at the time.

[`CLAUDE.md`](../../CLAUDE.md) has the rest of the convention, including when a
change warrants a record at all.
