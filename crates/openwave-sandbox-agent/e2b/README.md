# The OpenWave documents template on E2B

E2B provisions sandboxes from templates registered with an E2B account, not
from arbitrary OCI references. Publishing a template makes it public: any E2B
account can create sandboxes from it — by its opaque *template ID*. The human
alias is not portable: E2B resolves a custom template's alias only inside the
team that owns it, and only E2B's own base templates (`code-interpreter-v1`)
resolve by alias from anywhere (verified against `api.e2b.app`, 2026-08-04).
That is how a user who has pasted nothing but an
E2B API key still gets OpenWave's official documents image — LibreOffice, the
`exec` helper scripts, and the document skills' Python dependencies already
installed, so a document run needs no in-sandbox `pip install`.

The alias is version-suffixed — `openwave-documents-v0-26-0` — so publishing a
new image version cannot change what an older client gets. `E2B_TEMPLATE` in
[`crates/openwave-code-execution/src/e2b.rs`](../../openwave-code-execution/src/e2b.rs)
pins the template ID that alias resolves to, with the alias kept alongside in
its doc comment for legibility.

## What is here

- `e2b.Dockerfile` — the template's build definition: the published documents
  image by digest, plus E2B's expected `user` account.
- `e2b.toml` — the template's identity and sizing, for CLI commands that read
  it rather than taking arguments.

## Publishing

[`.github/workflows/publish-e2b-template.yml`](../../../.github/workflows/publish-e2b-template.yml)
does this. It runs on every push to `main` that touches this directory, reads
the alias and sizing out of `e2b.toml`, and:

1. asks E2B whether that alias already resolves and is public. If it does, the
   run logs it and stops — re-runs and edits to this README cost one API call;
2. otherwise builds the template from `e2b.Dockerfile` on E2B's own
   infrastructure and publishes it, making it creatable by any E2B account;
3. verifies the alias now resolves as public, then opens a PR bumping
   `E2B_TEMPLATE` in `e2b.rs` to the template ID it resolves to.

Because the alias is version-suffixed, the pin PR from a sandbox image publish
is what puts a new alias in `e2b.toml`, and merging that PR is what sets this
workflow going. Nothing else has to be done by hand.

**One-time setup.** The workflow needs an E2B API key for the OpenWave account
in the `E2B_API_KEY` Actions secret (Settings → Secrets and variables →
Actions), copied from the [E2B dashboard's Keys
tab](https://e2b.dev/dashboard?tab=keys). Only a repository admin can set it.
Without it the run fails and says so rather than skipping, because a silent
skip is indistinguishable from "nothing to do" and would leave every E2B user
on the `code-interpreter-v1` fallback with no signal. `E2B_ACCESS_TOKEN` (the
dashboard's Personal tab) is optional and only passed through for the few
team-scoped endpoints that still take it; the CLI's `template create` and
`template list` both require the API key.

Two things the workflow deliberately does not do:

- It will not republish an alias that is already public, so editing
  `e2b.Dockerfile` without bumping the alias in `e2b.toml` changes nothing on
  E2B. That is the right default — a published alias is a promise about
  contents — but when a rebuild under the same name really is what you want,
  dispatch the workflow manually with `force_rebuild` set.
- It cannot prove *cross-account* resolution, which is the whole point of
  publishing and the one thing a publish from inside the account does not
  demonstrate. Check it once, by hand, with a throwaway account's API key and
  the template ID from the run's summary (the alias would 404 from another
  account even when everything is right):

  ```sh
  E2B_API_KEY=<other-account-key> \
    e2b sandbox create <template-id>
  ```

### Doing it by hand

The fallback, for when the workflow is broken or the secret is not set yet.
Run from this directory, on a machine authenticated against the OpenWave E2B
account. `e2b auth login` uses a browser; `E2B_API_KEY` works headless.

```sh
npm install --global @e2b/cli@2.16.1    # or: brew install e2b
export E2B_API_KEY=<openwave-account-key>

# Build System 2.0 (current CLI). The build runs on E2B's infrastructure — no
# local Docker daemon. The name is the public alias; `create` takes no config
# file, so the sizing recorded in e2b.toml is passed explicitly.
e2b template create openwave-documents-v0-26-0 \
  --dockerfile e2b.Dockerfile --cpu-count 2 --memory-mb 2048

# Make it public. Without the argument the CLI would read e2b.toml's
# template_id, which is this same alias.
e2b template publish openwave-documents-v0-26-0
```

`e2b template list --format json` shows every template the account owns with a
`public` field; `e2b template unpublish openwave-documents-v0-26-0` reverses
the publish. The client tolerates an unpublished alias: sandbox creation falls
back to `code-interpreter-v1` when E2B cannot resolve the OpenWave template, in
a degraded mode where the document skills install their Python dependencies at
run time.

## Bumping the version

The image tag, the digest in `e2b.Dockerfile`, the alias in `e2b.toml`, and
`E2B_TEMPLATE` in `e2b.rs` move together — but not at the same moment, and each
step is a PR someone merges:

1. Publish the new sandbox image (`.github/workflows/publish-sandbox-image.yml`
   runs on version tags). Its `pin` job opens a PR that updates
   `e2b.Dockerfile`'s digest and tag comment and renames the alias in
   `e2b.toml` to match the new version, e.g. `openwave-documents-v0-27-0`.
   Merge it.
2. That merge touches this directory, so `publish-e2b-template.yml` builds and
   publishes the new alias, then opens the PR moving `E2B_TEMPLATE` onto the
   new template's ID. Merge that one too.

`E2B_TEMPLATE` moves last on purpose: until the template is actually published,
the new template resolves for nobody and every E2B user drops to
`code-interpreter-v1`. Leave the previous template published until clients
pinned to it are out of circulation — unpublishing it drops those users to the
same degraded fallback.

## Notes on the template

- E2B runs `envd` as PID 1 and ignores the image's `ENTRYPOINT`, so the sandbox
  agent binary the image carries is dormant on E2B. OpenWave's E2B provider
  talks to `envd` directly; the image is here for its contents.
- The template needs no start command: everything the document skills use is
  installed at image-build time and nothing has to be running.
- E2B's default sandbox user is `user` with `/home/user` as its home, which is
  also the workspace root the provider's file APIs address. The OpenWave image
  runs as `openwave` out of a different home, so the Dockerfile creates `user`
  explicitly rather than depending on E2B's builder to add it.
