# OS-managed policy (MDM)

How an organization points Tidebreak at its model gateway through device
management, and what Tidebreak reads to honor it. This page describes the
admin-visible artifacts; the resolution rules (precedence, validation, the
fail-closed misconfigured state) live in the module documentation of
`crates/tidebreak-server/src/managed_policy.rs`.

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

Each platform's artifact may also carry `DownloadUpdatesAutomatically`. It
sets **Settings → Updates → Download updates automatically** and locks it,
and the setting reads "Managed by your organization." When it is `false`,
Tidebreak still checks for updates and says when one is available, but it
downloads the update only when the person chooses **Download update**. When
it is `true`, automatic downloads stay on. Absent leaves the choice to the
person, and the setting starts on. A present-but-broken value fails closed to
`false`. The key needs no gateway: an artifact that carries only this key
leaves the profile unmanaged. The desktop app reads the key before every
update check, so a change applies without a restart.

## macOS — managed preferences

Deploy a configuration profile that forces a preference for the app's bundle
identifier:

- Domain: `io.brightwave.tidebreak` (release builds; debug builds read
  `io.brightwave.tidebreak.dev`)
- Key: `GatewayURL` (string)
- Key: `AllowLocalMcpServers` (boolean, optional; also accepted as the
  string `true`/`false`)
- Key: `DownloadUpdatesAutomatically` (boolean, optional; also accepted as
  the string `true`/`false`)

Tidebreak reads the forced-preferences domain that `cfprefsd` materializes
under `/Library/Managed Preferences`, honoring the user channel before the
device channel. Only root-owned files are honored, and a broken channel
falls through to the next one. User preferences (`defaults write`) are not
consulted — only MDM-forced values count.

## Windows — registry policy

Deploy (GPO or Intune) a machine-scoped registry value:

- Key: `HKLM\Software\Policies\Brightwave\Tidebreak`
- Value: `GatewayURL` (`REG_SZ`)
- Value: `AllowLocalMcpServers` (`REG_SZ`, optional; `true` or `false`)
- Value: `DownloadUpdatesAutomatically` (`REG_SZ`, optional; `true` or
  `false`)

The native 64-bit view of the hive is read explicitly.

## Linux — policy file

Install a JSON file:

- Path: `/etc/tidebreak/managed-policy.json`
- Schema: `{ "gateway_url": "https://gateway.example.com",
  "allow_local_mcp_servers": false,
  "download_updates_automatically": false }` (every key is optional, but the
  file must name at least one)

An absent file means no OS policy; an unreadable or malformed file resolves
managed-but-misconfigured, as above.

Deploy it root-owned and not world-writable, as `/etc` content should be.
Unlike the macOS reader, the file reader does not verify ownership today —
the permissions are deployment guidance, not an enforced guarantee.

## A gateway you connected through a link

A profile can also become managed without any device management. A gateway's
own web page offers a `tidebreak://provision` link, and the link raises a
full-window screen in Tidebreak. That screen says a link made the request,
names the gateway's host, and says what the gateway would control: which
models and tools you can use. It also says the gateway would hide your own
provider keys. **Not now** has the focus, so pressing Enter declines. Only
the sign-in that **Sign in and connect** starts commits the pairing, which
writes the provisioned policy file described in the following section.

**Settings → Model Gateway** names who put the gateway there. An OS-managed
profile reads "Managed by your organization's device policy," and offers no
way to leave, because only your administrator can change that policy. A
profile you paired reads "You connected this gateway on" and the date, and
its **Danger zone** offers **Leave gateway**.

To leave a gateway you connected, choose **Leave gateway** and confirm.
Tidebreak signs out of the gateway, deletes the provisioned policy, and
returns to the open profile with your own provider keys. The delete names
the gateway you confirmed, so a policy that changed in the meantime is left
alone. The payload-free `tidebreak://deprovision` link does the same after a
native confirmation. An OS-managed gateway refuses both.

## Developer flow — the provisioned policy file

There is no unmanaged gateway settings surface: the hand-typed gateway URL
field and the additive "use model gateway" toggle are retired, and policy is
the only way a profile becomes gateway-connected. To exercise the real
managed path against a local gateway without an MDM profile, quit Tidebreak
and write the same sticky provisioned state that deep-link pairing commits
when its sign-in completes:

```sh
umask 077
printf '%s\n' '{"gateway_url":"http://127.0.0.1:8081"}' \
  > "<data dir>/gateway-policy.json"
```

Restart Tidebreak. The profile starts managed (`source: provisioned`), and
sign-in, model sync, and routing use the gateway URL that you provided. The
settings panel reports the file's modification time as the day you connected
the gateway. To return to the open profile, choose **Leave gateway** in
**Settings → Model Gateway**, or quit Tidebreak and delete the file:

```sh
rm "<data dir>/gateway-policy.json"
```

Restart Tidebreak after deleting the file. An OS-managed (MDM) assertion
always outranks this file. A profile
provisioned to one gateway refuses a bare provision link for another;
opening such a link instead asks, in a native dialog naming both gateways,
whether to re-pair. Confirming parks the replacement, and completing a
sign-in against the new gateway commits it — the old gateway's session is
revoked and cleared in the same step. An OS-asserted gateway can never be
replaced by re-pairing or left from settings.
