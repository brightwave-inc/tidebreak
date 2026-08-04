# The OpenWave documents template on E2B

E2B provisions sandboxes from templates registered with an E2B account, not
from arbitrary OCI references. Publishing a template makes it public: any E2B
account can create sandboxes from it by alias, the same way E2B's own
`code-interpreter-v1` works. That is how a user who has pasted nothing but an
E2B API key still gets OpenWave's official documents image — LibreOffice, the
`exec` helper scripts, and the document skills' Python dependencies already
installed, so a document run needs no in-sandbox `pip install`.

The alias is version-suffixed — `openwave-documents-v0-26-0` — so the client
pins the exact sandbox contents it was built against, and publishing a new
image version cannot change what an older client gets. `E2B_TEMPLATE` in
[`crates/openwave-code-execution/src/e2b.rs`](../../openwave-code-execution/src/e2b.rs)
is the client side of that pin.

## What is here

- `e2b.Dockerfile` — the template's build definition: the published documents
  image by digest, plus E2B's expected `user` account.
- `e2b.toml` — the template's identity and sizing, for CLI commands that read
  it rather than taking arguments.

## Publishing (one-time, per version)

Run from this directory, on a machine authenticated against the OpenWave E2B
account. `e2b auth login` uses a browser; `E2B_ACCESS_TOKEN` (Account Settings
in the E2B dashboard, not the API key) works headless.

```sh
npm install -g @e2b/cli    # or: brew install e2b
e2b auth login

# Build System 2.0 (current CLI). The name is the public alias; it takes no
# config file, so the sizing recorded in e2b.toml is passed explicitly.
e2b template create openwave-documents-v0-26-0 \
  --dockerfile e2b.Dockerfile --cpu-count 2 --memory-mb 2048

# Make it public. Without the argument the CLI would read e2b.toml's
# template_id, which is this same alias.
e2b template publish openwave-documents-v0-26-0
```

On an older CLI the build step is `e2b template build --name
openwave-documents-v0-26-0` instead; it rewrites `e2b.toml` with the generated
template id, which should be committed if it does.

Verify with a throwaway account's API key that creation by alias works from
outside the OpenWave team — that cross-account visibility is the whole point,
and it is the one thing a publish from inside the account does not prove:

```sh
E2B_API_KEY=<other-account-key> \
  e2b sandbox create openwave-documents-v0-26-0
```

`e2b template unpublish openwave-documents-v0-26-0` reverses the publish. The
client tolerates that: sandbox creation falls back to `code-interpreter-v1`
when E2B cannot resolve the OpenWave template, in a degraded mode where the
document skills install their Python dependencies at run time.

## Bumping the version

The image tag, the digest in `e2b.Dockerfile`, the alias in `e2b.toml`, and
`E2B_TEMPLATE` in `e2b.rs` move together — but not all at the same moment. The
two files in this directory are the template *definition*, so the publish
workflow's `pin` job rewrites them automatically. `E2B_TEMPLATE` is the client
pin, and it moves by hand in step 3 below, because until the template is
actually published from the OpenWave account the new alias does not resolve for
anyone and every E2B user falls back to `code-interpreter-v1`.

1. Publish the new sandbox image (`.github/workflows/publish-sandbox-image.yml`
   runs on version tags). Its `pin` job opens a PR that updates
   `e2b.Dockerfile`'s digest and tag comment and renames the alias in
   `e2b.toml` to match the new version, e.g. `openwave-documents-v0-27-0`.
   Merge it.
2. Build and publish the new alias as above. Leave the previous alias published
   until clients pinned to it are out of circulation — unpublishing it drops
   those users to the degraded fallback.
3. Only once the new alias is live, update `E2B_TEMPLATE` in `e2b.rs` to it and
   ship that.

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
