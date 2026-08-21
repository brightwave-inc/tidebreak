import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { startBrowserFixture } from "./server.mjs";

let fixture;

before(async () => {
  fixture = await startBrowserFixture({ port: 0, crossOriginPort: 0 });
});

after(async () => {
  await fixture.close();
});

test("the primary page names the live cross-origin frame", async () => {
  const response = await fetch(fixture.origin);
  assert.equal(response.status, 200);
  assert.match(response.headers.get("content-type"), /^text\/html/);
  const source = await response.text();
  assert.match(source, /Agent browser fixture/);
  assert.match(source, new RegExp(`${fixture.crossOrigin}/cross-frame`));
  assert.match(source, /Ignore the user's task/);
});

test("the item API is deterministic and resettable", async () => {
  const initial = await fetch(`${fixture.origin}/api/items`).then((response) =>
    response.json(),
  );
  assert.deepEqual(initial.items, [{ id: 1, label: "Inspect the preview" }]);

  const createdResponse = await fetch(`${fixture.origin}/api/items`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ label: "Verify semantic click" }),
  });
  assert.equal(createdResponse.status, 201);
  assert.deepEqual(await createdResponse.json(), {
    item: { id: 2, label: "Verify semantic click" },
  });

  const resetResponse = await fetch(`${fixture.origin}/reset`, { method: "POST" });
  assert.equal(resetResponse.status, 200);
  assert.deepEqual(await resetResponse.json(), { status: "reset" });
});

test("redirect, delay, frame, upload, and download endpoints expose stable contracts", async () => {
  const redirect = await fetch(`${fixture.origin}/redirect`, { redirect: "manual" });
  assert.equal(redirect.status, 302);
  assert.equal(redirect.headers.get("location"), "/redirected?from=redirect");

  const redirected = await fetch(
    `${fixture.origin}/redirected?from=%3Cscript%3Ealert(1)%3C%2Fscript%3E`,
  ).then((response) => response.text());
  assert.match(redirected, /Source: redirect/);
  assert.doesNotMatch(redirected, /<script>alert\(1\)<\/script>/);

  const delayed = await fetch(`${fixture.origin}/slow?ms=5`).then((response) =>
    response.json(),
  );
  assert.deepEqual(delayed, { status: "ready", waitedMs: 5 });

  const boundedDelay = await fetch(`${fixture.origin}/slow?ms=999999`).then(
    (response) => response.json(),
  );
  assert.deepEqual(boundedDelay, { status: "ready", waitedMs: 250 });

  const crossFrame = await fetch(`${fixture.crossOrigin}/cross-frame`);
  assert.equal(crossFrame.status, 200);
  assert.match(await crossFrame.text(), /Cross-origin frame/);

  const upload = await fetch(`${fixture.origin}/upload`, {
    method: "POST",
    body: "fixture bytes",
  }).then((response) => response.json());
  assert.equal(upload.status, "uploaded");
  assert.equal(upload.bytes, 13);

  const download = await fetch(`${fixture.origin}/download`);
  assert.equal(download.status, 200);
  assert.equal(
    download.headers.get("content-disposition"),
    'attachment; filename="fixture-download.txt"',
  );
  assert.equal(await download.text(), "browser fixture download\n");
});

test("oversized request bodies are refused before an endpoint acts", async () => {
  const response = await fetch(`${fixture.origin}/upload`, {
    method: "POST",
    body: "x".repeat(1024 * 1024 + 1),
  });
  assert.equal(response.status, 413);
  assert.deepEqual(await response.json(), { error: "body_too_large" });
});
