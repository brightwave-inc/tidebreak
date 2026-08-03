# OS-managed policy (MDM)

How an organization points OpenWave at its model gateway through device
management, and what OpenWave reads to honor it. This page describes the
admin-visible artifacts; the resolution rules (precedence, validation, the
fail-closed misconfigured state) live in the module documentation of
`crates/openwave-server/src/managed_policy.rs`.

On every platform the asserted value is a gateway **base URL**: `http` or
`https`, no embedded credentials. A present-but-broken artifact — wrong
shape, wrong type, or a URL failing that contract — never falls back to the
open experience: the profile resolves managed-but-misconfigured with no
usable gateway, and the server logs a warning naming what is broken.

Each platform's artifact may also carry `AllowLocalMcpServers`. On a managed
profile manual MCP configuration is locked and gateway-managed endpoint
mounts are the only sanctioned path; this key is the org's explicit opt-out
for local tooling. When it is `true`, local stdio (`command`) MCP servers
are left to the user, while remote (`url`) servers remain locked. Absent
means `false`, and a present-but-broken value fails closed to deny.

## macOS — managed preferences

Deploy a configuration profile that forces a preference for the app's bundle
identifier:

- Domain: `io.brightwave.openwave` (release builds; debug builds read
  `io.brightwave.openwave.dev`)
- Key: `GatewayURL` (string)
- Key: `AllowLocalMcpServers` (boolean, optional; also accepted as the
  string `true`/`false`)

OpenWave reads the forced-preferences domain that `cfprefsd` materializes
under `/Library/Managed Preferences`, honoring the user channel before the
device channel. Only root-owned files are honored, and a broken channel
falls through to the next one. User preferences (`defaults write`) are not
consulted — only MDM-forced values count.

## Windows — registry policy

Deploy (GPO or Intune) a machine-scoped registry value:

- Key: `HKLM\Software\Policies\Brightwave\OpenWave`
- Value: `GatewayURL` (`REG_SZ`)
- Value: `AllowLocalMcpServers` (`REG_SZ`, optional; `true` or `false`)

The native 64-bit view of the hive is read explicitly.

## Linux — policy file

Install a JSON file:

- Path: `/etc/openwave/managed-policy.json`
- Schema: `{ "gateway_url": "https://gateway.example.com",
  "allow_local_mcp_servers": false }` (the second key is optional)

An absent file means no OS policy; an unreadable or malformed file resolves
managed-but-misconfigured, as above.

Deploy it root-owned and not world-writable, as `/etc` content should be.
Unlike the macOS reader, the file reader does not verify ownership today —
the permissions are deployment guidance, not an enforced guarantee.

## Developer flow — the sqlite policy toggle

There is no unmanaged gateway settings surface: the hand-typed gateway URL
field and the additive "use model gateway" toggle are retired, and policy is
the only way a profile becomes gateway-connected. To exercise the real
managed path against a local gateway without an MDM profile, write the same
sticky provisioned state that deep-link pairing commits when its sign-in
completes — the `managed_policy_v1` row in the profile database:

```sh
sqlite3 "<data dir>/openwave.db" \
  "INSERT INTO setting(key, value_json) VALUES
     ('managed_policy_v1', '{\"gateway_url\":\"http://127.0.0.1:8081\"}')
   ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json;"
```

Policy is re-resolved on every read, so the profile turns managed
(`source: provisioned`) on the next `/policy` read — sign-in, model sync,
and routing all run the genuine managed path against the URL you provided.
Delete the row to return to the open profile:

```sh
sqlite3 "<data dir>/openwave.db" \
  "DELETE FROM setting WHERE key = 'managed_policy_v1';"
```

Note that an OS-managed (MDM) assertion always outranks this row. A profile
provisioned to one gateway refuses a bare provision link for another;
opening such a link instead asks, in a native dialog naming both gateways,
whether to re-pair. Confirming parks the replacement, and completing a
sign-in against the new gateway commits it — the old gateway's session is
revoked and cleared in the same step. Deleting the row (above) remains the
way to return to the open profile; an OS-asserted gateway can never be
replaced by re-pairing.
