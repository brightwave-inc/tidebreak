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

test("the primary page covers verification privacy and ordinary numeric controls", async () => {
  const source = await fetch(fixture.origin).then((response) => response.text());

  assert.match(source, /name="code" inputmode="numeric"/);
  assert.match(source, /id="verification-code" inputmode="numeric"/);
  assert.match(source, /name="auth-digits" inputmode="numeric" maxlength="6"/);
  assert.match(source, /name="challenge-response" inputmode="numeric" pattern="\[0-9\]\{6\}"/);
  assert.match(source, /placeholder="Recovery code"/);
  assert.match(source, /role="textbox" contenteditable inputmode="numeric" aria-label="Verification code"/);
  assert.match(source, /id="split-code"/);
  assert.equal((source.match(/input aria-label="Digit [1-6]"/g) ?? []).length, 6);
  assert.match(source, /name="quantity" type="number"/);
  assert.match(source, /name="zipCode" inputmode="numeric" maxlength="5"/);
  assert.match(source, /name="year" type="number"/);
  assert.match(source, /name="search" inputmode="numeric"/);
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

// ── Wait condition contracts ────────────────────────────────────────

test("the fixture serves a delayed response on /slow for wait testing", async () => {
  const fast = await fetch(`${fixture.origin}/slow?ms=5`).then(
    (response) => response.json(),
  );
  assert.deepEqual(fast, { status: "ready", waitedMs: 5 });

  const moderate = await fetch(`${fixture.origin}/slow?ms=500`).then(
    (response) => response.json(),
  );
  assert.deepEqual(moderate, { status: "ready", waitedMs: 500 });
});

test("the fixture has stable titles for URL-change wait detection", async () => {
  const primary = await fetch(fixture.origin).then((response) => response.text());
  assert.match(primary, /<title>Agent browser fixture<\/title>/);

  const redirected = await fetch(`${fixture.origin}/redirected?from=redirect`).then(
    (response) => response.text(),
  );
  assert.match(redirected, /<title>Redirect complete<\/title>/);
});

test("the fixture serves popup-target with stable semantic content for text-presence waits", async () => {
  const popup = await fetch(`${fixture.origin}/popup-target`).then(
    (response) => response.text(),
  );
  assert.match(popup, /<title>Popup target<\/title>/);
  assert.match(popup, /Confirm popup/);
});

test("reset restores deterministic state for repeated wait tests", async () => {
  await fetch(`${fixture.origin}/api/items`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ label: "Pre-wait item" }),
  });

  const resetResponse = await fetch(`${fixture.origin}/reset`, {
    method: "POST",
  });
  assert.equal(resetResponse.status, 200);

  const items = await fetch(`${fixture.origin}/api/items`).then(
    (response) => response.json(),
  );
  assert.deepEqual(items.items, [{ id: 1, label: "Inspect the preview" }]);
});

test("cross-origin frame URL is stable for unsupported-frame contract testing", async () => {
  const source = await fetch(fixture.origin).then((response) => response.text());
  assert.match(source, new RegExp(`${fixture.crossOrigin}/cross-frame`));

  const crossFrame = await fetch(`${fixture.crossOrigin}/cross-frame`).then(
    (response) => response.text(),
  );
  assert.match(crossFrame, /Cross-origin frame/);
  assert.match(crossFrame, /<h1>Cross-origin frame<\/h1>/);
});

test("the delayed-content endpoint reveals content after timing for element-visible wait simulation", async () => {
  // Immediate: no content
  const before = await fetch(`${fixture.origin}/slow?ms=0`).then(
    (response) => response.json(),
  );
  assert.deepEqual(before, { status: "ready", waitedMs: 0 });

  // After moderate wait: available
  const after = await fetch(`${fixture.origin}/slow?ms=100`).then(
    (response) => response.json(),
  );
  assert.deepEqual(after, { status: "ready", waitedMs: 100 });
});

// ── Screenshot contract surface ─────────────────────────────────────

test("the fixture primary page is visual and returns a valid viewport-sized document", async () => {
  const source = await fetch(fixture.origin).then((response) => response.text());
  assert.match(source, /viewport/);
  assert.match(source, /width: min\(980px/);
  assert.match(source, /Dynamic items/);
});

// ── Typed error contracts ───────────────────────────────────────────

test("fixture endpoints return structured errors for unknown paths", async () => {
  const response = await fetch(`${fixture.origin}/nonexistent`);
  assert.equal(response.status, 404);
  assert.deepEqual(await response.json(), { error: "not_found" });
});

test("fixture refuses an empty label in add-item", async () => {
  const response = await fetch(`${fixture.origin}/api/items`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ label: "   " }),
  });
  assert.equal(response.status, 400);
  assert.deepEqual(await response.json(), { error: "label_required" });
});
