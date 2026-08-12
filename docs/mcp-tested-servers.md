# Tested and community MCP servers

Any MCP server can be mounted, and Tidebreak treats them all the same at
runtime: the same discovery bounds, the same approval gate, the same
namespacing. Server *quality* is not uniform, though, and a badly-schema'd
server makes the whole agent feel broken rather than the server. So the
Connected apps surface labels each configured server one of two ways.

- **Tested** — the server matches an entry on Tidebreak's curated list. Someone
  mounted that exact server, drove it, and dated the result.
- **Community** — everything else. Mounted, connected, and callable, exactly
  like a tested one. We simply have not driven it ourselves and will not imply
  otherwise.

**The label gates nothing.** It changes no approval, no tool exposure, and no
connection behavior. It exists so the two claims stay distinguishable.

## What "tested" claims

An entry means a person ran the server against a real Tidebreak profile and
exercised, at minimum:

- **The auth flow** — whatever the server needs to authenticate (a stdio
  server's forwarded environment names, or an HTTP server's bearer token
  variable), configured from Settings and reconnecting cleanly after a
  restart.
- **Tool schemas** — discovery completes inside Tidebreak's bounds (frame size,
  tool count, description and schema limits), the mounted names survive the
  `mcp__{namespace}__{tool}` contract, and the published schemas describe the
  arguments the server actually accepts.
- **Streaming** — responses arrive over both the plain-JSON and
  `text/event-stream` shapes the transport admits, and a long call does not
  strand the turn.
- **Approval previews** — the approval card for each tool reads as a sentence a
  person can decide on, rather than an opaque blob.

It does **not** claim the server is secure, well-maintained, or run by anyone
we have a relationship with. It is a report on one session, with a date.

## What the list holds

The list lives in `crates/tidebreak-server/src/mcp_curated.rs`, next to the
model registry it is modelled on: a static table compiled into the app, no
network fetch, no background refresh. Each entry carries a display name, the
pattern that recognises the server, the date it was last exercised, and a
sentence of notes.

Recognition is deliberately narrow, because a loose pattern would hand the
badge to a server nobody tested:

- a **stdio** entry matches the executable's file stem — case-insensitively, so
  a path, a bare name, and a Windows `.exe` all land on the same entry — plus
  the leading arguments that select the server's MCP mode;
- an **HTTP** entry matches the URL's exact `scheme://authority`. The whole
  authority is compared, so `https://curated.example@evil.example/` cannot
  borrow a curated origin.

A gateway-backed mount matches nothing here: its endpoint is resolved from the
signed-in gateway session at connect time and no command or URL is stored, so
the honest answer is the community label.

The tier is computed from the *saved* definition on every read. While a server
row is being edited and not yet saved, its label still describes the saved
definition.

## Adding an entry

1. Mount the server in a real profile and work through the four contracts
   above. If any of them is broken, the outcome is a bug report, not an entry.
2. Add a row to `CURATED_MCP_SERVERS` with today's date and notes that say what
   you drove — the notes are shown to the reader deciding how much the badge is
   worth, so "verified" is not a useful sentence.
3. Keep the pattern as narrow as it can be while still matching how people
   normally configure that server.

Re-exercise an entry when the server ships a major version and move the date
forward, or drop the entry. A stale date is more honest than a fresh label
nobody re-earned, but an entry no one is willing to re-drive should come out.
