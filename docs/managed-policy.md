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

## macOS — managed preferences

Deploy a configuration profile that forces a preference for the app's bundle
identifier:

- Domain: `io.brightwave.openwave` (release builds; debug builds read
  `io.brightwave.openwave.dev`)
- Key: `GatewayURL` (string)

OpenWave reads the forced-preferences domain that `cfprefsd` materializes
under `/Library/Managed Preferences`, honoring the user channel before the
device channel. Only root-owned files are honored, and a broken channel
falls through to the next one. User preferences (`defaults write`) are not
consulted — only MDM-forced values count.

## Windows — registry policy

Deploy (GPO or Intune) a machine-scoped registry value:

- Key: `HKLM\Software\Policies\Brightwave\OpenWave`
- Value: `GatewayURL` (`REG_SZ`)

The native 64-bit view of the hive is read explicitly.

## Linux — policy file

Install a root-owned JSON file:

- Path: `/etc/openwave/managed-policy.json`
- Schema: `{ "gateway_url": "https://gateway.example.com" }`

An absent file means no OS policy; an unreadable or malformed file resolves
managed-but-misconfigured, as above.
