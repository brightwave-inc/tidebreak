# 53. Code Worktrees Live in a User-Visible, Configurable Root

- Status: Accepted
- Date: 2026-08-20
- Owners: code mode
- Related: [`0032-code-workspaces-worktrees-checkpoints.md`](0032-code-workspaces-worktrees-checkpoints.md),
  [`0030-code-mode-separate-surface.md`](0030-code-mode-separate-surface.md),
  [`0045-run-code-mode-on-windows.md`](0045-run-code-mode-on-windows.md),
  [`docs/code-mode.md`](../code-mode.md)

## Context

[`0032`](0032-code-workspaces-worktrees-checkpoints.md) rooted every worktree
under the Tidebreak data directory, and
[`0045`](0045-run-code-mode-on-windows.md) restated that Windows introduces no
user-selectable location. On the desktop that data directory is platform app
data keyed by the bundle identifier, so a workspace lands in
`~/Library/Application Support/io.brightwave.tidebreak[.dev|.staging]/code/worktrees/<repoId>-<slug>/<workspaceId>-<slug>/`.

Four facts make that the wrong home, and none of them was known to be wrong
when 0032 was written — worktrees were an implementation detail then, and they
are user work now:

- A worktree holds uncommitted changes on a real branch. App data is
  conventionally disposable: uninstall flows and "reset app data" delete it,
  and a user consenting to either is not consenting to lose a day's work.
- `~/Library` is hidden in Finder and in file dialogs, and the UUID-prefixed
  segments make the paths long and unreadable everywhere they show up — shell
  prompts, editor titles, and every `cd` an agent narrates.
- The bundle identifier keys the path, so each channel grows its own tree; a
  development machine carries three copies of the same person's work.
- Everything an agent builds inside — one current tree is 2.4 GB, mostly
  `node_modules` — is backed up as application state.

The database, logs, blobs, and managed tools are correctly placed. Only the
worktrees are misfiled. Clone destinations already solved the same problem with
the `code_clone_parent_dir` setting, which is the pattern to copy rather than
invent.

## Decision

**The root is a setting.** `code_worktree_root` is one deployment-wide setting,
read and written on the deployment plane behind `require_admin` — the same
plane the clone parent lives on, for the same reason: it decides where every
principal's work lands on this machine. A multi-user deployment inserts the
per-owner path segment clones already use, so two users' same-named
repositories cannot share a folder.

**The default depends on the embedding, not the profile.** The boot config
carries `code_worktree_root_default`. The desktop app sets it to
`~/Tidebreak/workspaces` — one root for all three channels, because a dev build
growing a second copy of the user's work is the problem, not the protection.
Embeddings that leave it absent — the CLI, self-host, tests — keep
`<data_dir>/code/worktrees`, which is right for a headless deployment whose
data directory *is* the operator-visible location and whose `TIDEBREAK_DATA_DIR`
is already a deliberate choice. A stored setting overrides either.

**Names lead, ids trail.** A worktree is created at
`<root>/<repo-slug>/<workspace-slug>-<short-id>/`. The workspace id, shortened
to eight hex digits, is a suffix rather than the leading segment: it still
carries uniqueness — titles repeat and may be empty — but it no longer costs
the reader the first thing they see. The repo segment is the slug alone;
workspace ids are unique across every repo, so two repos that share a name
share a folder without ever sharing a worktree.

**The root applies to new workspaces only.** A git worktree records absolute
paths in two places — its own `.git` file and the repository's
`.git/worktrees/*` entry — so relocating one is a `git worktree repair` pass,
not a rename. Every existing workspace keeps the absolute `worktree_path` on
its row and its checkout stays exactly where it is. Nothing recomputes a path
for a row that already has one.

Deliberately excluded: a mover or repair pass for existing trees, a per-repo
override (still deferred), and any change to where the database, blobs, logs,
or managed tools live.

## Alternatives Considered

**Do nothing.** Rejected: the location is not a cosmetic complaint. Losing
uncommitted branches to a documented "reset app data" step is a data-loss path,
and it is reached by a user doing something the platform tells them is safe.

**Move existing worktrees and repair them on first boot.** Rejected for this
change. `git worktree repair` is the correct mechanism, but a boot-time pass
that moves every user's checkouts — across channels, with sessions possibly
mid-turn and agent processes holding open files — is a much larger risk than
the problem it closes. The setting makes the migration possible later; leaving
existing trees in place makes this change safe now. A user who wants an old
workspace in the new root can archive it and create a new one.

**Derive the default from `Profile::Desktop`.** Rejected: the CLI runs the
desktop profile too, and a headless `tidebreak serve` should not start writing
into `~/Tidebreak`. The embedding that owns a window is the one that knows a
visible home directory is the right answer, so the embedding names it.

**Put worktrees beside the clone parent (`<clone-parent>/../worktrees`).**
Rejected: the clone parent is wherever the user keeps source checkouts, which
is often a directory they curate; deriving a sibling from it would put
Tidebreak-managed trees inside someone's `~/src` layout without asking.

## Consequences

Two conventions now exist on one machine: workspaces created before this change
sit in app data, and workspaces created after sit in the visible root. The
paths are absolute and stored, so both keep working, but a user's workspace list
spans two locations until the old ones are archived. That is the accepted cost
of not writing a mover.

`~/Tidebreak/workspaces` is a directory Tidebreak creates in the user's home.
It is created when the first worktree lands, not at boot, so an install that
never opens code mode leaves nothing behind.

Worktrees are no longer inside the data directory the desktop marks private to
the host broker, so a user *can* now attach a workspace folder as a chat-mode
folder. That is a user choice about their own source tree, and the same choice
they already have for any clone.

Revisit if the two-location split proves confusing enough to justify the repair
pass, or if a per-repo override lands and the deployment-wide root stops being
the only answer.

## Validation

- Path construction: the readable segments lead, the short id is the suffix,
  and two workspaces on one repo with the same title resolve to different
  directories.
- The headless default is byte-for-byte the old `<data_dir>/code/worktrees`, so
  an existing self-host deployment sees no move.
- End to end: create a workspace, move the root, create a second workspace, and
  assert the first workspace's stored path and its checkout are both unchanged
  while the second lands under the new root. A wrong implementation that
  recomputed paths on read would pass a test that only checked the new
  workspace, which is why the assertion on the *old* row is the load-bearing
  one.
- Setting a root that does not exist creates it; a relative path and a path
  that names a file are refused when set, not at the first workspace that
  fails.
- The route is admin-gated by registration: a member is refused, an admin is
  not.
