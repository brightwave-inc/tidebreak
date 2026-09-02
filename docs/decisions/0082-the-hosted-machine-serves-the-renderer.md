# 82. The hosted machine serves the renderer

- Status: Accepted
- Date: 2026-09-02
- Owners: server, desktop
- Related: [`0006-self-host-deployment-plane-authorization.md`](0006-self-host-deployment-plane-authorization.md),
  [`0047-gateway-linked-hosting.md`](0047-gateway-linked-hosting.md),
  [`../self-hosting.md`](../self-hosting.md)
- Supersedes: none

## Context

A hosted Tidebreak machine has no page. The server mounts the API, the view
frames, the auth discovery document, and `/healthz`; its root answers `404`.
The only client is the desktop app, which attaches to the machine with a
Gateway-minted bearer ([record 47](0047-gateway-linked-hosting.md)).
[`docs/deferred.md`](../deferred.md) parks a hosted web UI as "a later
surface on that same wire".

That later surface is now the one that matters. A Model Gateway administrator
who sets the machine up in the console has nowhere to go from there: the
console can name the machine's URL, and the URL shows nothing. The desktop app
is the wrong first answer for that person — they are already in a browser,
signed in, one click from the machine.

The renderer that would serve them already exists. The desktop UI is a React
bundle the packaged app loads over Tauri's own protocol; every attachment
state a browser tab could be in is one the app already renders, including the
remote attachment with host authority struck through ([PR
#2514](https://github.com/brightwave-inc/tidebreak/pull/2514) made
`attachedRemotely` the signal for that).

## Decision

The self-host image builds the desktop renderer and the server serves it.

1. **The image carries the bundle.** `deploy/self-host/Dockerfile` gains a
   Node stage that runs the desktop UI's `pnpm build` and copies `dist` into
   the runtime image at `/opt/tidebreak/ui`. Node 22 and pnpm 10 match the
   desktop UI lane in CI; the base image is digest-pinned like the others.
   The build installs with `--ignore-scripts`: a production bundle loads no
   native module, and the image build should not run the dependency tree's
   lifecycle hooks.

2. **The server serves it only when told to.** `TIDEBREAK_UI_DIST` names the
   bundle directory. Set, the server mounts it as the router's fallback:
   files by name, and `index.html` for a `GET` or `HEAD` whose `Accept`
   names `text/html` and whose path no route claimed. A `fetch` for an
   unknown route keeps its `404`, and every API route is matched first, so a
   navigation to `/chats` is still the bearer check's `401`. Unset, nothing
   changes: an unknown path is a `404`, not a `401` and not a page. The
   directory is verified at bind, and a configured directory without an
   `index.html` refuses to start.

3. **The bundle is the one the desktop ships.** The same `dist` Tauri
   packages as `frontendDist`, with no hosted build flavor. Boot gains one
   branch: outside Tauri, with no explicit URL and no dev listen endpoint, a
   production bundle asks its own origin for `/auth/discovery`. A discovery
   document means the page is served by a machine; the page then attaches to
   `window.location.origin` as a remote attachment, with `gatewayAuth` set
   when the machine authenticates through a gateway.

4. **The bearer arrives in the fragment, once.** A page served by a machine
   holds whatever bearer it was handed in `#handoff=<token>`. Boot takes it
   out of the address before the router exists, keeps it in memory, and
   nowhere else — no cookie, no storage. A page that opens without one shows
   a sign-in screen that names the gateway console, because the console is
   what can mint one. The mint and the redeem are the gateway's and a
   follow-up route's; this record settles only what the page does with the
   result.

5. **A browser session lasts its token.** The desktop refreshes its bearer
   from the gateway session it holds; a browser tab holds no such session,
   so nothing refreshes. When the machine stops accepting the bearer, the
   page says the session ended and sends the reader back through the
   console. A refresh path for long-lived tabs is deferred until someone
   keeps one open past an hour and minds.

6. **The Machine panel knows a tab has nowhere to go.** The desktop offers to
   disconnect from a remote machine and return to the server inside the app.
   A browser tab has no such server, so the offer is replaced by what the
   reader can do: close the tab, or attach from the desktop app.

## Consequences

- The hosted machine has a landing page, which is what the console's
  Manage action needs to link to.
- The server image grows by the bundle and its build time by the Node stage.
  The Docker context now admits the desktop UI package directory; the
  deny-by-default `Dockerfile.dockerignore` still excludes `node_modules`, a
  stale `dist`, and every hidden file, and the context probe test pins that.
- Origin checks do not change. The self-host profile does not check
  `Origin` — an operator-chosen name is not knowable to the server — and a
  same-origin page never triggers CORS. A hosted tab and the machine share
  one origin by construction.
- Storybook carries the hosted browser states: "Shell/Hosted browser
  session" and the Machine panel's in-browser variant.

## Alternatives considered

- **A separate hosted web app.** A second renderer would drift from the
  desktop's within a release. The desktop already renders every attachment
  state a tab needs.
- **A cookie session.** The server reads bearers from `Authorization` and the
  WebSocket subprotocol only; a cookie would be the first in the codebase
  and would need CSRF protection across every mutating route.
- **Serving `index.html` for every unmatched path.** Swallows API `404`s: a
  renderer newer than its server would parse a page as JSON instead of
  learning an endpoint is missing.
