# Decision records

Numbered records of decisions that later work has to live with. Each states
what was chosen, what was rejected, and what would cause it to be revisited.

The files in this directory are the index — they sort by number and their names
say what they decide. Nothing lists them elsewhere, so nothing goes stale.

Numbers are unique. Once a record has merged, its number is permanent. An
unmerged draft may be renumbered when another branch takes its number first.
Gaps are allowed; filling an older hole is not.

Use the repository commands rather than copying a neighboring file:

```console
# Picks the next number and fills 0000-template.md.
python3 scripts/adr.py new --title "Decision title" --owners core

# Unique numbers and a current manifest.
python3 scripts/adr.py check

# Before pushing, also check whether latest main assigned this number elsewhere.
python3 scripts/adr.py check --base origin/main

# Resolve that collision; --number is optional and defaults to one past the highest.
python3 scripts/adr.py renumber docs/decisions/0076-decision-title.md
```

`python3 scripts/adr.py sync` regenerates the intentionally conflict-prone
[manifest](manifest.txt). Two branches that add the same next number both
append to it, so the second cannot merge until someone renumbers. A conflict
there means another branch took your number: keep main's record, renumber
yours, then rerun `python3 scripts/adr.py check`. Do not hand-edit the
manifest.

Merging is the acceptance; a record still under discussion says `Proposed`.
When a decision changes, the old record gets `Status: Superseded` and a
pointer to its replacement rather than being rewritten — the value is the
reasoning at the time.

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
