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

Records preserve the names, paths, and operational context that existed when
the decision was accepted. In particular, records predating the Tidebreak
rename may use the former OpenWave name. Current documentation and source code
use Tidebreak terminology; the historical wording is not a second product or
an active compatibility alias.

Write one before building when a change fixes something later work has to live
with: a data-model or ownership boundary, a wire or persisted contract, a
vocabulary that will spread through the codebase, or a rule two subsystems will
both be held to. Ordinary implementation, bug fixes, and work whose shape an
existing record already settles go straight to an issue or a PR.

Record the alternatives you rejected and why, and say what would make you
revisit the decision. A record that states only the chosen design cannot stop
the same argument being reopened.
