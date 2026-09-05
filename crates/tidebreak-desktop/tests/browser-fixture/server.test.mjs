import assert from "node:assert/strict";
import { createHash } from "node:crypto";
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
  const source = await fetch(fixture.origin).then((response) =>
    response.text(),
  );

  assert.match(source, /name="code" inputmode="numeric"/);
  assert.match(source, /id="verification-code" inputmode="numeric"/);
  assert.match(source, /name="auth-digits" inputmode="numeric" maxlength="6"/);
  assert.match(
    source,
    /name="challenge-response" inputmode="numeric" pattern="\[0-9\]\{6\}"/,
  );
  assert.match(source, /placeholder="Recovery code"/);
  assert.match(
    source,
    /role="textbox" contenteditable inputmode="numeric" aria-label="Verification code"/,
  );
  assert.match(source, /blockquote data-sensitive-descendant/);
  assert.match(source, /id="split-code"/);
  assert.equal(
    (source.match(/input aria-label="Digit [1-6]"/g) ?? []).length,
    6,
  );
  assert.match(source, /name="quantity" type="number"/);
  assert.match(source, /name="zipCode" inputmode="numeric" maxlength="5"/);
  assert.match(source, /name="year" type="number"/);
  assert.match(source, /name="search" inputmode="numeric"/);
});

test("the primary page exposes every same-document navigation sequence", async () => {
  const source = await fetch(fixture.origin).then((response) =>
    response.text(),
  );

  assert.match(source, /history\.pushState\(\{\}, "", link\.href\)/);
  assert.match(
    source,
    /history\.replaceState\(\{\}, "", "\/\?view=replaced"\)/,
  );
  assert.match(source, /location\.hash = location\.hash === "#summary"/);
  assert.match(source, /addEventListener\("popstate", renderRoute\)/);
  assert.match(source, /addEventListener\("hashchange", renderRoute\)/);
  assert.match(source, /history\.back\(\)/);
  assert.match(source, /history\.forward\(\)/);
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

  const resetResponse = await fetch(`${fixture.origin}/reset`, {
    method: "POST",
  });
  assert.equal(resetResponse.status, 200);
  assert.deepEqual(await resetResponse.json(), { status: "reset" });
});

test("cross-origin redirects stay pinned to the paired fixture", async () => {
  for (const query of [
    "",
    "?url=https://example.invalid/&destination=/download",
  ]) {
    const response = await fetch(
      fixture.origin + "/redirect-cross-origin" + query,
      {
        redirect: "manual",
      },
    );
    assert.equal(response.status, 302);
    assert.equal(
      response.headers.get("location"),
      fixture.crossOrigin + "/cross-frame",
    );
    assert.equal(response.headers.get("cache-control"), "no-store");
    assert.equal(await response.text(), "");
  }

  const post = await fetch(fixture.origin + "/redirect-cross-origin", {
    method: "POST",
    redirect: "manual",
  });
  assert.equal(post.status, 404);
  assert.equal(post.headers.get("location"), null);
});

test("redirect, delay, frame, upload, and download endpoints expose stable contracts", async () => {
  const redirect = await fetch(`${fixture.origin}/redirect`, {
    redirect: "manual",
  });
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
  const fast = await fetch(`${fixture.origin}/slow?ms=5`).then((response) =>
    response.json(),
  );
  assert.deepEqual(fast, { status: "ready", waitedMs: 5 });

  const moderate = await fetch(`${fixture.origin}/slow?ms=500`).then(
    (response) => response.json(),
  );
  assert.deepEqual(moderate, { status: "ready", waitedMs: 500 });
});

test("the fixture has stable titles for URL-change wait detection", async () => {
  const primary = await fetch(fixture.origin).then((response) =>
    response.text(),
  );
  assert.match(primary, /<title>Agent browser fixture<\/title>/);

  const redirected = await fetch(
    `${fixture.origin}/redirected?from=redirect`,
  ).then((response) => response.text());
  assert.match(redirected, /<title>Redirect complete<\/title>/);
});

test("the fixture serves popup-target with stable semantic content for text-presence waits", async () => {
  const popup = await fetch(`${fixture.origin}/popup-target`).then((response) =>
    response.text(),
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

  const items = await fetch(`${fixture.origin}/api/items`).then((response) =>
    response.json(),
  );
  assert.deepEqual(items.items, [{ id: 1, label: "Inspect the preview" }]);
});

test("cross-origin frame URL is stable for unsupported-frame contract testing", async () => {
  const source = await fetch(fixture.origin).then((response) =>
    response.text(),
  );
  assert.match(source, new RegExp(`${fixture.crossOrigin}/cross-frame`));

  const crossFrame = await fetch(`${fixture.crossOrigin}/cross-frame`).then(
    (response) => response.text(),
  );
  assert.match(crossFrame, /Cross-origin frame/);
  assert.match(crossFrame, /<h1>Cross-origin frame<\/h1>/);
});

test("the delayed-content endpoint reveals content after timing for element-visible wait simulation", async () => {
  // Immediate: no content
  const before = await fetch(`${fixture.origin}/slow?ms=0`).then((response) =>
    response.json(),
  );
  assert.deepEqual(before, { status: "ready", waitedMs: 0 });

  // After moderate wait: available
  const after = await fetch(`${fixture.origin}/slow?ms=100`).then((response) =>
    response.json(),
  );
  assert.deepEqual(after, { status: "ready", waitedMs: 100 });
});

// ── Screenshot contract surface ─────────────────────────────────────

test("the fixture primary page is visual and returns a valid viewport-sized document", async () => {
  const source = await fetch(fixture.origin).then((response) =>
    response.text(),
  );
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

async function downloadState(origin, token, expected) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const response = await fetch(origin + "/api/slow-download?token=" + token, {
      signal: AbortSignal.timeout(2000),
    });
    assert.equal(response.status, 200);
    const state = await response.json();
    if (!expected || state.status === expected) return state;
    await new Promise((done) => setTimeout(done, 10));
  }
  assert.fail("controlled download did not reach " + expected);
}

test("the recovery page and module have a separate stable route", async () => {
  const response = await fetch(fixture.origin + "/recovery");
  assert.equal(response.status, 200);
  const source = await response.text();
  assert.match(source, /<title>Browser recovery fixture<\/title>/);
  assert.match(source, /type="module" src="\/recovery.mjs"/);
  const module = await fetch(fixture.origin + "/recovery.mjs");
  assert.equal(module.status, 200);
  assert.match(module.headers.get("content-type"), /^text\/javascript/);
  assert.match(await module.text(), /export function mountRecoveryPage/);
});

test("controlled downloads expose an active prefix and finish only after release", async () => {
  const token = "release-contract";
  const response = await fetch(
    fixture.origin + "/slow-download?token=" + token,
    { signal: AbortSignal.timeout(5000) },
  );
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-length"), "65536");
  assert.equal(
    response.headers.get("content-disposition"),
    'attachment; filename="fixture-recovery-release-contract.txt"',
  );
  const reader = response.body.getReader();
  const first = await reader.read();
  assert.equal(
    Buffer.from(first.value).toString(),
    "browser fixture recovery download\n",
  );
  let complete = false;
  const body = (async () => {
    const chunks = [Buffer.from(first.value)];
    while (true) {
      const next = await reader.read();
      if (next.done) break;
      chunks.push(Buffer.from(next.value));
    }
    complete = true;
    return Buffer.concat(chunks);
  })();
  assert.deepEqual(await downloadState(fixture.origin, token), {
    token,
    status: "waiting",
    requests: 1,
    totalBytes: 65536,
    prefixBytes: 34,
    timeoutMs: 120000,
  });
  assert.equal(complete, false);
  const released = await fetch(
    fixture.origin + "/api/slow-download?token=" + token,
    { method: "POST" },
  );
  assert.equal(released.status, 200);
  assert.ok(
    ["releasing", "completed"].includes((await released.json()).status),
  );
  const bytes = await body;
  assert.equal(bytes.length, 65536);
  assert.equal(bytes.subarray(34).equals(Buffer.alloc(65536 - 34, "r")), true);
  assert.equal(
    (await downloadState(fixture.origin, token, "completed")).requests,
    1,
  );
  const replay = await fetch(fixture.origin + "/slow-download?token=" + token);
  assert.equal(replay.status, 409);
  assert.equal((await replay.json()).error, "download_token_reused");
  assert.equal((await downloadState(fixture.origin, token)).requests, 2);
  assert.equal(
    (
      await fetch(fixture.origin + "/api/slow-download?token=" + token, {
        method: "POST",
      })
    ).status,
    409,
  );
});

test("aborted downloads remain terminal and release cannot complete them", async () => {
  const token = "abort-contract";
  const controller = new AbortController();
  const response = await fetch(
    fixture.origin + "/slow-download?token=" + token,
    { signal: controller.signal },
  );
  const reader = response.body.getReader();
  assert.equal((await reader.read()).done, false);
  controller.abort();
  await assert.rejects(reader.read());
  const aborted = await downloadState(fixture.origin, token, "aborted");
  assert.equal(aborted.requests, 1);
  const release = await fetch(
    fixture.origin + "/api/slow-download?token=" + token,
    { method: "POST" },
  );
  assert.equal(release.status, 409);
  assert.equal((await release.json()).status, "aborted");
});

test("controlled downloads time out with a truncated body and cannot be released later", async () => {
  const token = "timeout-contract";
  const response = await fetch(
    fixture.origin + "/slow-download?token=" + token + "&timeout_ms=250",
    { signal: AbortSignal.timeout(5000) },
  );
  await assert.rejects(response.arrayBuffer());
  const timedOut = await downloadState(fixture.origin, token, "timed_out");
  assert.equal(timedOut.timeoutMs, 250);
  assert.equal(
    (
      await fetch(fixture.origin + "/api/slow-download?token=" + token, {
        method: "POST",
      })
    ).status,
    409,
  );
});

test("download tokens, timeout values, release bodies, and concurrent transfers are bounded", async () => {
  for (const query of [
    "token=",
    "token=" + "a".repeat(65),
    "token=bad%0Avalue",
    "token=invalid-timeout&timeout_ms=120001",
    "token=invalid-timeout&timeout_ms=0",
    "token=invalid-timeout&timeout_ms=NaN",
  ]) {
    assert.equal(
      (await fetch(fixture.origin + "/slow-download?" + query)).status,
      400,
    );
  }
  assert.equal(
    (await fetch(fixture.origin + "/api/slow-download?token=missing")).status,
    404,
  );
  const controllers = [];
  try {
    for (let index = 0; index < 4; index += 1) {
      const controller = new AbortController();
      controllers.push(controller);
      const response = await fetch(
        fixture.origin + "/slow-download?token=concurrent-" + index,
        { signal: controller.signal },
      );
      assert.equal(response.status, 200);
      assert.equal((await response.body.getReader().read()).done, false);
    }
    assert.equal(
      (await fetch(fixture.origin + "/slow-download?token=concurrent-overflow"))
        .status,
      503,
    );
    const badRelease = await fetch(
      fixture.origin + "/api/slow-download?token=concurrent-0",
      { method: "POST", body: "unexpected" },
    );
    assert.equal(badRelease.status, 400);
    assert.equal(
      (await downloadState(fixture.origin, "concurrent-0")).status,
      "waiting",
    );
  } finally {
    for (const controller of controllers) controller.abort();
    for (let index = 0; index < controllers.length; index += 1) {
      await downloadState(fixture.origin, "concurrent-" + index, "aborted");
    }
  }
});

test("fixture shutdown closes held downloads without waiting for their timeout", async () => {
  const isolated = await startBrowserFixture({ port: 0, crossOriginPort: 0 });
  const response = await fetch(
    isolated.origin + "/slow-download?token=shutdown",
    { signal: AbortSignal.timeout(5000) },
  );
  const body = response.arrayBuffer();
  const rejected = assert.rejects(body);
  await isolated.close();
  await rejected;
});

test("multipart uploads report the exact attached file bytes without exposing contents", async () => {
  const content = Buffer.from("browser fixture download\n");
  const form = new FormData();
  form.append("note", "do not report ordinary fields");
  form.append(
    "file",
    new Blob([content], { type: "text/plain" }),
    "fixture-download.txt",
  );
  const response = await fetch(`${fixture.origin}/upload`, {
    method: "POST",
    body: form,
  });
  assert.equal(response.status, 200);
  const result = await response.json();
  assert.deepEqual(result.files, [
    {
      name: "fixture-download.txt",
      bytes: 25,
      sha256: createHash("sha256").update(content).digest("hex"),
    },
  ]);
  assert.equal(
    JSON.stringify(result).includes(content.toString("utf8")),
    false,
  );
  assert.equal(
    JSON.stringify(result).includes("do not report ordinary fields"),
    false,
  );
});
