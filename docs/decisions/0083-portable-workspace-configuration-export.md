# 83. Portable workspace configuration export

- Status: Accepted
- Date: 2026-09-02
- Owners: core
- Related: [`0032-code-workspaces-worktrees-checkpoints.md`](0032-code-workspaces-worktrees-checkpoints.md),
  [`0027-native-authorization-for-local-mcp-commands.md`](0027-native-authorization-for-local-mcp-commands.md)
- Supersedes: none

## Context

Workspace setup today lives only in the current Tidebreak installation. Code
repository registrations (`CodeRepo`) and MCP server definitions
(`McpServerDefinition`) have no documented portable file, so moving to another
machine means repeating that setup by hand.

A portable file must reconstruct those two sections without copying secrets,
credentials, transcripts, or worktrees. Some fields are device-specific
(`root_path`, `command`, `cwd`) and must be remapped on import rather than
applied blindly. Managed-profile lockdown already refuses new or edited
manual MCP transports; import must obey the same rule.

This slice is only those two sections. Connected apps, folders, plugins,
providers, and preferences stay out of the file.

## Decision

**Envelope.** Export writes a JSON document:

```json
{
  "tidebreak_config": 1,
  "exported_at": "<RFC 3339 timestamp>",
  "sections": {
    "code_repositories": [ /* … */ ],
    "mcp_servers": [ /* … */ ]
  }
}
```

`tidebreak_config` is the format version integer. Readers refuse a newer
version with an actionable error (export again from a matching Tidebreak, or
upgrade this install). Older versions, when they exist, are migrated in the
reader. Unknown envelope keys and unknown section names are refused with a
message that names the unknown key and says to use a file Tidebreak exported.

**Code repositories.** Each entry carries `display_name`, `origin_url` (git
`origin` remote when the checkout answers, otherwise `cloned_from`),
`root_path`, `default_base_ref`, `branch_prefix`, `setup_script`,
`archive_script`, `quick_actions`, and `cloned_from`. `root_path` is
device-specific. Ids, owners, timestamps, worktree paths, and transcript
paths are never written.

**MCP servers.** Each entry carries `name`, `command`, `args`, `env` (names
only), `env_from`, `cwd`, `url`, `bearer_token_env` (the environment *name*),
`gateway_endpoint`, `request_timeout_ms`, and `enabled`. Plugin-sourced
servers are omitted: the plugin tree rebuilds them. `env_values`, bearer
token values, and any other secret are never written. `command` and `cwd` are
device-specific.

**Import preview.** The preview classifies each entry as `new`, `identical`,
or `conflict` (same repository origin URL, or same MCP server name, with
different portable fields; the response lists those field names). Independently
it reports `needs_remap` for device-specific values that do not exist here:
missing `root_path`, unresolvable `command`, missing `cwd`.

**Import apply.** Apply takes the document plus per-entry decisions: `skip`,
`add`, `replace`, and optional remapped values. Nothing is overwritten unless
the user chose `replace` for that entry. Managed-profile lockdown is the same
admission check `PUT /mcp/servers` already uses.

**Secrets.** The file is not a credential store. Import never writes secret
values that were not in the file, which they never are.

## Alternatives Considered

**Dump the whole data directory.** Would copy transcripts, worktrees, and
secret-store files. Rejected: this slice is configuration, not a machine
clone.

**TOML or a tarball of JSON fragments.** A single versioned JSON envelope is
one file the user can keep in git, and unknown keys fail closed.

**Silent overwrite on import.** Rejected: preview plus explicit replace is
the only way to avoid clobbering a workspace the user already set up.

## Consequences

Adding a section later is a format-version bump, or a named key the current
reader must refuse until it understands it. Device-specific remaps stay in
the import UI; they are not guessed.

Revisit when another settings family (providers, connected apps, folders)
needs the same envelope: bump the format version and add a section rather
than inventing a second file.

## Validation

Export tests assert the JSON contains no `env_values`, no bearer token
values, and no transcript or worktree path keys. Preview tests cover `new`,
`identical`, `conflict`, and `needs_remap`. Apply tests refuse overwrite
without `replace`. A newer `tidebreak_config` is refused with a message that
tells the reader what to do.
