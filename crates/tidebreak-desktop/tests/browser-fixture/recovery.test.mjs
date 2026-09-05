import assert from "node:assert/strict";
import { test } from "node:test";
import {
  mountRecoveryPage,
  readRecoveryMarkers,
  recoveryCookieName,
  recoveryStorageKey,
  saveRecoveryMarker,
} from "./recovery.mjs";

function browserState() {
  const values = new Map();
  const writes = [];
  const cookieWrites = [];
  let cookies = "unrelated_fixture_cookie=unchanged";
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem(key, value) {
      writes.push([key, value]);
      values.set(key, value);
    },
  };
  const makeDocument = () => {
    const elements = new Map();
    for (const id of [
      "fixture-marker",
      "local-storage-marker",
      "cookie-marker",
      "recovery-status",
      "slow-download",
      "save-recovery-marker",
      "read-recovery-markers",
    ]) {
      elements.set("#" + id, {
        value: "",
        textContent: "",
        attributes: new Map(),
        listeners: new Map(),
        setAttribute(name, value) {
          this.attributes.set(name, value);
        },
        removeAttribute(name) {
          this.attributes.delete(name);
        },
        addEventListener(name, listener) {
          this.listeners.set(name, listener);
        },
      });
    }
    return {
      querySelector: (selector) => elements.get(selector),
      get cookie() {
        return cookies;
      },
      set cookie(value) {
        cookieWrites.push(value);
        cookies = "unrelated_fixture_cookie=unchanged; " + value.split(";")[0];
      },
    };
  };
  return {
    storage,
    values,
    writes,
    cookieWrites,
    makeDocument,
    clearProfile() {
      values.clear();
      cookies = "";
    },
  };
}

function click(doc, id) {
  doc.querySelector("#" + id).listeners.get("click")();
}

test("page load only reads markers and an explicit save survives a fresh page", () => {
  const state = browserState();
  const first = state.makeDocument();
  mountRecoveryPage(first, state.storage);
  assert.equal(
    first.querySelector("#local-storage-marker").textContent,
    "Local storage marker: Missing",
  );
  assert.equal(
    first.querySelector("#cookie-marker").textContent,
    "Cookie marker: Missing",
  );
  assert.deepEqual(state.writes, []);
  assert.deepEqual(state.cookieWrites, []);
  assert.equal(
    first.querySelector("#slow-download").attributes.has("href"),
    false,
  );

  first.querySelector("#fixture-marker").value = "restart-run_2345";
  click(first, "save-recovery-marker");
  assert.deepEqual(state.writes, [[recoveryStorageKey, "restart-run_2345"]]);
  assert.deepEqual(state.cookieWrites, [
    recoveryCookieName +
      "=restart-run_2345; Max-Age=604800; Path=/; SameSite=Lax",
  ]);
  assert.equal(
    first.querySelector("#recovery-status").textContent,
    "Recovery marker saved.",
  );
  assert.equal(
    first.querySelector("#slow-download").attributes.get("href"),
    "/slow-download?token=restart-run_2345",
  );

  const restored = state.makeDocument();
  mountRecoveryPage(restored, state.storage);
  assert.equal(
    restored.querySelector("#local-storage-marker").textContent,
    "Local storage marker: restart-run_2345",
  );
  assert.equal(
    restored.querySelector("#cookie-marker").textContent,
    "Cookie marker: restart-run_2345",
  );
  assert.equal(
    restored.querySelector("#fixture-marker").value,
    "restart-run_2345",
  );
  assert.equal(state.writes.length, 1);
  assert.equal(state.cookieWrites.length, 1);
});

test("clearing profile storage displays Missing without reseeding", () => {
  const state = browserState();
  saveRecoveryMarker(state.storage, state.makeDocument(), "reset-run-2345");
  state.clearProfile();
  const reset = state.makeDocument();
  mountRecoveryPage(reset, state.storage);
  assert.equal(
    reset.querySelector("#local-storage-marker").textContent,
    "Local storage marker: Missing",
  );
  assert.equal(
    reset.querySelector("#cookie-marker").textContent,
    "Cookie marker: Missing",
  );
  assert.equal(reset.querySelector("#fixture-marker").value, "");
  assert.equal(state.writes.length, 1);
  assert.equal(state.cookieWrites.length, 1);
});

test("invalid or oversized markers cannot write storage or inject a cookie", () => {
  const state = browserState();
  const doc = state.makeDocument();
  for (const value of [
    "",
    "a".repeat(65),
    "marker; other=changed",
    "<script>",
    "has space",
  ]) {
    assert.throws(() => saveRecoveryMarker(state.storage, doc, value), /1–64/);
  }
  assert.deepEqual(state.writes, []);
  assert.deepEqual(state.cookieWrites, []);
  assert.equal(doc.cookie, "unrelated_fixture_cookie=unchanged");
});

test("marker reads ignore unrelated cookies and reject ambiguous or malformed marker cookies", () => {
  const state = browserState();
  assert.deepEqual(
    readRecoveryMarkers(state.storage, "another=fixture-value"),
    { localStorage: null, cookie: null },
  );
  assert.throws(
    () => readRecoveryMarkers(state.storage, recoveryCookieName + "=%ZZ"),
    /invalid/,
  );
  assert.throws(
    () =>
      readRecoveryMarkers(
        state.storage,
        recoveryCookieName + "=one; " + recoveryCookieName + "=two",
      ),
    /ambiguous/,
  );
  assert.deepEqual(state.writes, []);
});

test("unavailable storage is reported without claiming the profile was cleared", () => {
  const state = browserState();
  const doc = state.makeDocument();
  mountRecoveryPage(doc, {
    getItem() {
      throw new Error("Storage is unavailable.");
    },
  });
  assert.equal(
    doc.querySelector("#local-storage-marker").textContent,
    "Local storage marker: Unavailable",
  );
  assert.equal(
    doc.querySelector("#cookie-marker").textContent,
    "Cookie marker: Unavailable",
  );
  assert.deepEqual(state.writes, []);
});
