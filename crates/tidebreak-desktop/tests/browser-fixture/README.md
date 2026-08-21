# Agent-browser fixture

Deterministic local pages for the in-app browser and semantic-driver tests.
The fixture has no package dependencies and binds only to loopback.

Run it:

```sh
node crates/tidebreak-desktop/tests/browser-fixture/server.mjs
```

The server prints one JSON line containing the primary and cross-origin URLs.
The defaults are `http://127.0.0.1:41781` and
`http://127.0.0.1:41782`. Override them with `--port` and
`--cross-origin-port`; pass `0` in programmatic tests to request ephemeral
ports.

Run the endpoint contract:

```sh
node --test crates/tidebreak-desktop/tests/browser-fixture/server.test.mjs
```

The primary page deliberately includes:

- history-API navigation and a server redirect;
- dynamic content and a replaceable stale target;
- ordinary, password, OTP, select, checkbox, upload, and submit controls;
- same-origin and cross-origin frames;
- a popup and a download;
- a console-error trigger;
- page-authored text that attempts to instruct the agent.

The fixture is test data, never an instruction source. Tests should assert that
the browser returns the injection text as untrusted page content.
