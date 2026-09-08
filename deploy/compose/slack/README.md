# Slack adapter beside a standalone machine

Run the published Tidebreak server image, its PostgreSQL store, and the Slack
adapter against the same Compose project. This is an operator example, not a
hosted add-on. Put a TLS-terminating reverse proxy in front of both published
ports. Nothing here terminates TLS.

Read [self-hosting](../../../docs/self-hosting.md) for the machine itself and
the Slack section there for the app manifest, bootstrap bearer, service
principal, and workspace grant. Read [Slack sessions](../../../docs/slack-sessions.md)
for how the adapter behaves once it is up.

## What you run

- `tidebreak`: `ghcr.io/brightwave-inc/tidebreak-server`, listening on container
  port 8080, published at `127.0.0.1:8080`.
- `postgres`: the machine's store.
- `slack-adapter`: `ghcr.io/brightwave-inc/tidebreak-slack-adapter` (tags
  `v<version>`), listening on container port 8080, published at
  `127.0.0.1:8081`.
- `adapter-postgres`: the adapter's own store. The adapter is a shared,
  stateful service; do not point it at the machine's database.

The adapter needs no Model Gateway variables.

## Files you create

Copy the examples, then fill them. Keep secrets `0600` and out of git.

```sh
cd deploy/compose/slack
umask 077
cp .env.example .env
cp slack.env.example slack.env
cp machines.json.example machines.json
cp tokens.example tokens
```

1. **`.env`** — database passwords, blob store, provider key, public machine
   URL, image tags, the adapter bootstrap bearer, and the token-sealing key.
2. **`slack.env`** — `SLACK_BOT_TOKEN` and `SLACK_SIGNING_SECRET`. The
   adapter receives events and slash commands over HTTPS request URLs; it
   does not use socket mode.
3. **`tokens`** — one person `admin` line and one `service` line. Channel
   sessions run as the service principal
   ([decision 0089](../../../docs/decisions/0089-service-principals.md)).
4. **`machines.json`** — the machine directory. `base_url` is the Compose
   service name (`http://tidebreak:8080`). `public_url` is the URL people open
   in a browser. `bootstrap_token` must match a value in
   `TIDEBREAK_ADAPTER_BOOTSTRAP_TOKENS`.

Generate tokens and the bootstrap bearer with `openssl rand -hex 32`.

## The machine directory: file or environment

This Compose file mounts `./machines.json` read-only and points
`SLACK_ADAPTER_MACHINES_FILE` at that path. Adapter images from gateway
release v0.1.0-alpha.195 read that file; set `SLACK_ADAPTER_MACHINES` to the
same JSON instead if you pin an older image.

To pass the same document as environment instead, drop the volume and set:

```text
SLACK_ADAPTER_MACHINES={"machines":{"standalone":{"kind":"standalone","base_url":"http://tidebreak:8080","public_url":"https://<your machine host>","bootstrap_token":"<the bearer>"}},"defaults":{"<Slack workspace id>":"standalone"}}
```

Do not set both unless the image you pin says that is valid.

## Start

```sh
docker compose --env-file .env up -d
curl -fsS http://127.0.0.1:8080/healthz
curl -fsS http://127.0.0.1:8081/health/setup
```

`GET /health/setup` on the adapter reports `ready` and `missing`. Fill
everything `missing` names before you invite the app into a workspace.

Point Slack event and slash-command request URLs at the adapter's public
origin, not at the machine. Point browsers at the machine's public origin for
the workspace grant approval.
