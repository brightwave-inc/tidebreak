import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../../src/browser_semantics.rs", import.meta.url), "utf8");
const match = source.match(/const BROWSER_GHOST_CURSOR_SCRIPT: &str = r#"([\s\S]*?)"#;/);
assert.ok(match, "the native page decoration must be available");
const markerKey = Symbol.for("io.brightwave.tidebreak.browser.observed-document");

function fixture() {
  const nodes = [];
  const timers = new Map();
  const frames = new Map();
  let nextId = 0;
  const node = (tag) => ({
    tag, style: {}, children: [], attributes: {},
    setAttribute(name, value) { this.attributes[name] = value; },
    appendChild(child) { this.children.push(child); },
    attachShadow(options) { this.shadowOptions = options; this.shadow = node("shadow"); return this.shadow; },
    remove() { this.removed = true; },
  });
  const document = {
    documentElement: node("html"),
    createElement(tag) { const result = node(tag); nodes.push(result); return result; },
    createElementNS(_namespace, tag) { return node(tag); },
  };
  const context = vm.createContext({
    document,
    window: { innerWidth: 800, innerHeight: 600 },
    location: { href: "https://fixture.invalid/" },
    setTimeout(fn) { const id = ++nextId; timers.set(id, fn); return id; },
    clearTimeout(id) { timers.delete(id); },
    requestAnimationFrame(fn) { const id = ++nextId; frames.set(id, fn); return id; },
    cancelAnimationFrame(id) { frames.delete(id); },
  });
  context[markerKey] = "snapshot-1";
  const show = (changes = {}) => JSON.parse(vm.runInContext(match[1].replace("__PAYLOAD__", JSON.stringify({
    id: "action-1", documentEpoch: 1, snapshotMarker: "snapshot-1",
    url: context.location.href, point: { x: 100, y: 120 },
    viewportWidth: 800, viewportHeight: 600, visibleMillis: 1200, action: "click", ...changes,
  })), context));
  const current = () => vm.runInContext('globalThis[Symbol.for("io.brightwave.tidebreak.browser.ghost-cursor")]', context);
  return { context, nodes, timers, frames, show, current };
}

test("ghost stays outside input and accessibility targets", () => {
  const { context, nodes, show, current } = fixture();
  assert.equal(show(), "shown");
  const host = nodes[0];
  assert.equal(host.attributes["aria-hidden"], "true");
  assert.match(host.style.cssText, /pointer-events:none!important/);
  assert.equal(host.shadowOptions.mode, "closed");
  const cursor = host.shadow.children[0];
  assert.equal(cursor.style.left, "96px");
  assert.equal(cursor.style.top, "117px");
  assert.equal(current().id, "action-1");
});

test("late old-action cleanup cannot remove a later cursor", () => {
  const { context, nodes, show, current } = fixture();
  show();
  show({ id: "action-2" });
  assert.equal(nodes[0].removed, true);
  show({ clear: true, id: "action-1" });
  assert.equal(current().id, "action-2");
  assert.equal(nodes[1].removed, undefined);
  show({ clear: true, id: null });
  assert.equal(current(), undefined);
  assert.equal(nodes[1].removed, true);
});

test("same-URL replacement documents cannot receive an old action marker", () => {
  const { context, nodes, show, current } = fixture();
  context[markerKey] = "snapshot-2";
  assert.equal(show(), "stale");
  assert.equal(nodes.length, 0);
});

test("navigation, viewport change, and replacement snapshot clear the cursor", () => {
  for (const changed of ["url", "viewport", "snapshot"]) {
    const { context, nodes, frames, show, current } = fixture();
    show();
    if (changed === "url") context.location.href += "#next";
    if (changed === "viewport") context.window.innerWidth = 700;
    if (changed === "snapshot") context[markerKey] = "snapshot-2";
    [...frames.values()][0]();
    assert.equal(nodes[0].removed, true, changed);
    assert.equal(current(), undefined, changed);
  }
});

test("expiry removes the decoration without sending input", () => {
  const { context, nodes, timers, show, current } = fixture();
  show();
  [...timers.values()][0]();
  assert.equal(nodes[0].removed, true);
  assert.equal(current(), undefined);
});
