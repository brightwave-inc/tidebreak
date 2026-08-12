# Folder bindings for local apps

The design contract for #1331: local apps gain a second binding kind that
grants a sandboxed app frame bounded access to a user-connected folder,
riding the host-access system ([host-access.md](host-access.md)) the same
way operation bindings ride connected apps
([connected-apps.md](connected-apps.md)). This page records the decisions;
implementation follows in slices.

The product case is the desktop-native app class nothing else serves: a
finance dashboard over a folder of bank exports, a triage board that reads
and files a messy directory, flashcards over a notes folder — apps over
data that will never be uploaded anywhere, built by asking the agent.

## The binding

```
{ "folder": "<root id>", "access": "read" | "read_write" }
```

A manifest binds an **approved connected folder** by its broker root id —
the stable, persisted identity a folder receives when the user connects it
through the trusted native picker. Folders are deliberately **not** a
connected-app kind: there is no record to CRUD, no namespace, no catalog —
the broker registration *is* the configuration, and inventing a shadow
record would put two names on one consent. The accepted asymmetry runs the
other way from the gateway's ("a filesystem MCP server is not an app"):
here the folder is first-class and no app record pretends to wrap it.

`create_app`'s roster gains a folders section (id + display name of each
approved folder) so the model can author bindings; like the rest of the
roster this is legibility, not the gate. `access` is part of the binding —
the model must declare write intent at authoring time, so the consent
sheet can say it before anything runs.

## Consent posture

Decided (2026-08-05), from the options recorded in #1331:

- **Co-grantable behind a combined-consent warning, not mutually
  exclusive.** A manifest may bind folders *and* REST operations. When it
  binds at least one folder and at least one operations binding, the
  consent sheet renders an explicit exfiltration warning naming both
  sides: *"This app can read 'Tax documents 2025' and send data to
  'Issues'."* Display names only — the projection stays names-only, and
  the leak tests keep it that way. The warning is the containment claim's
  replacement on mixed grants: the MVP's "an app can render lies but
  cannot exfiltrate" becomes "cannot exfiltrate what you were not
  explicitly warned it could read and send", and that trade was made
  deliberately for the read-local-then-post app class (the receipt filer,
  the notes syncer). With tool bindings retired (#1332), "network-capable"
  needs no transport analysis: every operations binding is network.
- **Write is in scope, as a louder consent line.** `read` grants listing
  and reading; `read_write` adds bounded writes and renders as its own
  warning-styled line on the sheet, never folded into the read line. A
  folder-writing app is also the interim answer to app state (the state
  primitive stays a non-goal): a kanban that persists its board writes a
  file in the folder the user granted it.
- One folder binding per root id per manifest, same as one binding per
  connected app.

Sequencing note, recorded honestly: relaxing exclusion into a warning
would have been a compatible widening; the reverse — tightening this
warning posture into exclusion later — invalidates granted apps. The
posture is chosen while granted users are ~zero, when either direction is
still cheap.

## Fingerprint

Folder grant bindings pin, in the standard canonical-JSON form:

```json
{"v":2,"kind":"folder","root_id":"<uuid>","access":"read"|"read_write"}
```

- `root_id` is the broker's persisted identity. Disconnecting or
  forgetting the folder removes it from the current-fingerprint lookup, so
  every grant naming it fails closed to `consent_required` — consent never
  outlives the registration it named. Reconnecting the same directory
  mints a new root id and therefore a fresh consent, which is correct: the
  approval chain was broken.
- `access` is consent-bearing: an app revision that upgrades `read` to
  `read_write` changes the fingerprint *and* exceeds the grant by
  construction, re-prompting either way.
- Display names are excluded (derived from the path basename, not
  consent), and paths never enter the form — the fingerprint must not be a
  path oracle, matching the invariant that the renderer and the model see
  opaque ids and display names only. Physical identity (device/inode)
  stays the broker's job: it already refuses I/O on a
  renamed-and-replaced directory, which is enforcement at use, not
  consent-time state.

## Enforcement and dispatch

The invoke route's shape is unchanged: pin check (the current revision
binds this folder at this access), grant gate (a live grant covers it and
**every** granted binding's fingerprint is current — folder entries join
the same all-or-nothing staleness check as connected apps), then dispatch.
The frame bridge gains `fs/list`, `fs/read`, and `fs/write` verbs
(capability-by-presence, like `operations/call`), and the invoke body a
third surface: `{ folder, op, path, ... }`.

Dispatch crosses a new seam, because today **nothing in tidebreak-server
can reach the broker** — folder tools are client-executed contracts the
desktop resolves, and the broker is a sidecar only the desktop process
spawns:

- `AppState` gains an optional host-folder handle (the
  `ExecFolderGrantResolver` / `rest_dispatch` pattern), injected by the
  desktop over its broker client, `None` in every other embedding.
- Absent the handle — headless `tidebreak serve`, generic embeddings — the
  door is honest at every layer, copying the
  `root_attachment_routes_enabled` precedent rather than the folder
  tools' park-forever one: the roster lists no folders, consent on a
  folder binding conflicts ("connected folders require the desktop app"),
  and invoke refuses. Nothing parks, nothing hangs.

Responsibilities split cleanly across the two consent systems rather than
merging them:

- **The app grant (server) is the whole app-level policy** — which app,
  which folder, which access, fingerprint-pinned, revocable from the
  library, exactly like operation bindings.
- **The broker keeps the host-level facts**: the folder is still
  registered (not forgotten, not set aside), its physical identity still
  matches, and every transfer respects its bounds — reads refuse over 64
  KiB text / 8 MiB binary, listings and writes keep their existing caps,
  binary crosses as base64 exactly like REST results.

The broker therefore learns a small **app-scoped surface on its trusted
control channel** (registration-checked, bounds-enforced, no conversation
attachment) rather than a new grant subject. The alternative — extending
`GrantSubject` with an app kind and relaxing the attachment precondition
in `authorize()` — was considered and rejected for now: it would duplicate
the app grant into the broker's ledger (two records of one consent, able
to disagree) and touch the persisted state format for no enforcement the
server-side gate does not already provide. Recorded as revisitable if the
broker ever becomes the sole consent store. The broker's audit trail still
sees every folder operation, attributed to a dedicated **app actor**
carrying the app id — not a borrowed grant subject — with writes recording
a durable intent before any bytes change, exactly like agent writes.

App writes are the first live user of the broker's bounded write
operation (chat writebacks resolve paths advisorily and journal through
the turn instead — a path that needs a chat and a turn, which an
out-of-turn invoke has neither of). Under a `read_write` grant, `create`
and `replace` are both allowed: the standing app grant *is* the write
consent, at folder granularity — the per-replacement native approval
stays a chat-writeback concept. Writes are capped at the broker's
existing byte bound and land atomically.

## Non-goals

- **No path-subtree scoping in v1.** Bindings are root-granularity; the
  broker's `PathSubtree` scope exists if apps ever need narrower grants.
- **No exec.** `ExecuteCommands` is not reachable from any app surface.
- **No folder management inside apps.** Connecting, widening, and
  disconnecting folders stay on the native Folders surface; an app can
  only use what is already approved and granted.
- **No chat coupling.** An app grant does not attach the folder to any
  conversation; chat attachments and their revision machinery are
  untouched.
- **No headless folder bindings** until something spawns the broker
  outside the desktop.

## Slices

1. **Vocabulary**: `AppBinding::Folder` / grant twin + fingerprint,
   shared grammar (one binding per root, access enum), `create_app` door
   refusal when no folders exist, docs.
2. **Server gate**: the host-folder seam on `AppState`, folder entries in
   the current-fingerprint lookup, consent-route folder arm with the
   desktop-required conflict, grant-state projection (folder lines,
   `access`, the combined-warning flag).
3. **Desktop + broker**: the app-scoped control-channel surface (list /
   read / write within bounds), the seam implementation over the broker
   client, roster wiring.
4. **Invoke + bridge**: the third invoke surface and dispatch, `fs/*`
   bridge verbs, `AppFrame` wiring, consent sheet rendering (write line,
   combined warning), wire regeneration.
5. **Dogfood**: a real file-based app end to end; what it teaches feeds
   back before any widening.
