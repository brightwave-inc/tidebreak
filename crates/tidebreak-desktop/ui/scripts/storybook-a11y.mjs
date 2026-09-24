// Checks every story in the built Storybook for serious and critical
// accessibility violations.
//
// The Storybook a11y addon runs axe after each story renders and attaches the
// result to the story's `storyFinished` event. This script serves the static
// build, opens each story in headless Chromium, reads that result, and fails
// on violations the allowlist does not name.
//
// Rendering and layout can shift on a busy machine, so the script checks the
// stories behind any failure a second time. A problem fails the run only if
// both passes show it.
//
//   pnpm storybook:build
//   pnpm storybook:a11y [--filter <story id text>] [--workers <count>]
//
// On a machine without Playwright's Chromium, install it once with
// `pnpm exec playwright-core install --only-shell chromium`.

import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { availableParallelism } from "node:os";
import { extname, join, normalize, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { chromium } from "playwright-core";
import { allowlist } from "./storybook-a11y-allowlist.mjs";

const staticRoot = fileURLToPath(new URL("../storybook-static", import.meta.url));
const allowlistFile = "scripts/storybook-a11y-allowlist.mjs";

const FAILING_IMPACTS = new Set(["critical", "serious"]);

// The allowlist's name for a story that ends without an axe result: it threw,
// its play function failed, or it timed out.
const STORY_ERROR = "story-error";

// Rules that judge the whole document. A story renders one component inside
// Storybook's iframe page, so these rules test that page, not the component.
// The addon already turns off `region` for the same reason.
const PAGE_LEVEL_RULES = new Set([
  "bypass",
  "document-title",
  "html-has-lang",
  "html-lang-valid",
  "html-xml-lang-mismatch",
  "landmark-one-main",
  "meta-refresh",
  "meta-viewport",
  "page-has-heading-one",
  "region",
]);

// A story that loads large viewers (PDF, spreadsheet, editor) can take several
// seconds to settle before axe runs on it.
const STORY_TIMEOUT_MS = 60_000;

const MIME_TYPES = {
  ".css": "text/css; charset=utf-8",
  ".gif": "image/gif",
  ".html": "text/html; charset=utf-8",
  ".ico": "image/x-icon",
  ".jpeg": "image/jpeg",
  ".jpg": "image/jpeg",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ttf": "font/ttf",
  ".txt": "text/plain; charset=utf-8",
  ".wasm": "application/wasm",
  ".webp": "image/webp",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
};

const { values: options } = parseArgs({
  options: {
    filter: { type: "string" },
    workers: { type: "string" },
  },
});
const workerCount = Math.max(
  1,
  Number.parseInt(options.workers ?? "", 10) || Math.min(4, availableParallelism()),
);

const indexFile = join(staticRoot, "index.json");
if (!existsSync(indexFile)) {
  console.error("No built Storybook found. Run `pnpm storybook:build` first.");
  process.exit(1);
}
checkAllowlistShape();

const index = JSON.parse(await readFile(indexFile, "utf8"));
const allStories = Object.values(index.entries).filter(
  (entry) => entry.type === "story",
);
// Storybook's `!test` tag is how a story opts out of every test run.
const untested = allStories.filter((story) => !story.tags?.includes("test"));
const stories = allStories
  .filter((story) => story.tags?.includes("test"))
  .filter((story) => !options.filter || story.id.includes(options.filter))
  .sort((a, b) => a.id.localeCompare(b.id));
if (stories.length === 0) {
  console.error("No stories to check.");
  process.exit(1);
}

// Every outcome each story produced, one per pass that checked it.
const attempts = new Map(stories.map((story) => [story.id, []]));
const started = Date.now();
const server = await serve(staticRoot);
const baseUrl = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch();
let result;
try {
  await checkAll(stories);
  result = evaluate();
  const recheck = stories.filter(
    (story) =>
      result.failures.some((failure) => failure.story === story) ||
      result.unused.some((entry) => matchesStory(entry.story, story.id)),
  );
  if (recheck.length > 0) {
    await checkAll(recheck);
    result = evaluate();
  }
} finally {
  await browser.close();
  server.close();
}

for (const { story, problems } of result.failures) {
  console.log(`\nFAIL ${story.id} (${story.title} > ${story.name})`);
  for (const problem of problems) {
    console.log(problem.replace(/^/gm, "    "));
  }
}
for (const { story, problems } of result.flaky) {
  console.log(`\nFLAKY ${story.id}: one pass of two showed ${problems.join(", ")}`);
}

// An entry that matched nothing in either pass is probably stale: the problem
// it excused is gone. Some violations depend on render timing, so this is a
// prompt to delete the entry, not a failure. Only a complete run with no
// failure can say so, because a story that stops rendering also stops
// matching its violation entries.
const stale = options.filter || result.failures.length > 0 ? [] : result.unused;
for (const entry of stale) {
  console.log(
    `\nSTALE allowlist entry: story "${entry.story}", rule "${entry.rule}". ` +
      `It matched nothing in this run; if the problem is fixed, delete it from ${allowlistFile}.`,
  );
}

const seconds = Math.round((Date.now() - started) / 1000);
const unmatched = `${stale.length} ${stale.length === 1 ? "entry" : "entries"} matched nothing`;
console.log(
  `\nChecked ${stories.length} stories in ${seconds}s with ${workerCount} workers: ` +
    `${result.failures.length} failed, ${result.flaky.length} flaky; ` +
    `allowlisted ${result.allowlistedViolations} violations and ` +
    `${result.allowlistedErrors} story errors, and ${unmatched}; ` +
    `${untested.length} opted out with the !test tag.`,
);
if (result.failures.length > 0) {
  console.log(
    `\nFix the story or component, or add a { story, rule, reason } entry to ${allowlistFile} ` +
      "for a known problem that cannot be fixed in this change.",
  );
  process.exit(1);
}

// Sorts every story's outcomes into failures, flaky problems, and allowlist
// use. A problem is keyed by its rule, so two passes compare rule by rule; an
// allowlist entry counts as used if any pass matched it.
function evaluate() {
  const used = new Set();
  const failures = [];
  const flaky = [];
  let allowlistedViolations = 0;
  let allowlistedErrors = 0;
  for (const story of stories) {
    const passes = attempts.get(story.id);
    if (passes.length === 0) continue;
    const found = passes.map((outcome) => problemsIn(story, outcome, used));
    const latest = found.at(-1);
    allowlistedViolations += latest.allowlistedViolations;
    allowlistedErrors += latest.allowlistedErrors;
    const everyPass = [...latest.problems].filter(([rule]) =>
      found.every((pass) => pass.problems.has(rule)),
    );
    const onePass = new Set(found.flatMap((pass) => [...pass.problems.keys()]));
    for (const [rule] of everyPass) onePass.delete(rule);
    if (everyPass.length > 0) {
      failures.push({ story, problems: everyPass.map(([, text]) => text) });
    }
    if (onePass.size > 0) {
      flaky.push({ story, problems: [...onePass] });
    }
  }
  const unused = options.filter ? [] : allowlist.filter((entry) => !used.has(entry));
  return { failures, flaky, unused, allowlistedViolations, allowlistedErrors };
}

function problemsIn(story, outcome, used) {
  const problems = new Map();
  let allowlistedViolations = 0;
  let allowlistedErrors = 0;
  const excuse = (rule) => {
    const entry = allowlist.find(
      (candidate) => candidate.rule === rule && matchesStory(candidate.story, story.id),
    );
    if (entry) used.add(entry);
    return Boolean(entry);
  };
  if (outcome.failure) {
    if (excuse(STORY_ERROR)) {
      allowlistedErrors += 1;
    } else {
      problems.set(STORY_ERROR, `${STORY_ERROR}: ${outcome.failure}`);
    }
  }
  for (const violation of outcome.violations ?? []) {
    if (!FAILING_IMPACTS.has(violation.impact) || PAGE_LEVEL_RULES.has(violation.id)) {
      continue;
    }
    if (excuse(violation.id)) {
      allowlistedViolations += 1;
    } else {
      problems.set(violation.id, describeViolation(violation));
    }
  }
  return { problems, allowlistedViolations, allowlistedErrors };
}

async function checkAll(list) {
  const queue = [...list];
  await Promise.all(
    Array.from({ length: Math.min(workerCount, queue.length) }, () => checkStories(queue)),
  );
}

async function checkStories(queue) {
  const context = await browser.newContext({
    viewport: { width: 1280, height: 800 },
    colorScheme: "dark",
    reducedMotion: "reduce",
  });
  await context.addInitScript(recordStoryOutcome);
  await context.addInitScript(settleTransitions);
  let page = await context.newPage();
  try {
    for (let story = queue.shift(); story; story = queue.shift()) {
      try {
        attempts.get(story.id).push(await checkStory(page, story));
      } catch (error) {
        attempts.get(story.id).push({
          failure: `The story did not finish: ${error.message.split("\n")[0]}`,
        });
        // A story that hangs or crashes leaves its page unusable, so the next
        // story starts on a fresh one.
        await page.close().catch(() => {});
        page = await context.newPage();
      }
    }
  } finally {
    await context.close();
  }
}

async function checkStory(page, story) {
  const url = `${baseUrl}/iframe.html?id=${encodeURIComponent(story.id)}&viewMode=story`;
  await page.goto(url, { waitUntil: "domcontentloaded" });
  await page.waitForFunction(() => globalThis.__tidebreakA11y?.done, undefined, {
    timeout: STORY_TIMEOUT_MS,
    polling: 100,
  });
  return page.evaluate(() => globalThis.__tidebreakA11y);
}

// Runs in the page before Storybook's own scripts. Axe reads colors as they
// stand when the story ends, so a transition still running reads as the wrong
// colors, such as a ghost button still fading in from its disabled opacity
// after its data loads. Transitions end at once here, so every check sees the
// settled state. Animations keep running: Radix waits for them to end before
// it removes content, and a story must still reach its real state.
function settleTransitions() {
  const install = () => {
    const style = document.createElement("style");
    style.textContent =
      "*, *::before, *::after { transition-duration: 0s !important; transition-delay: 0s !important; }";
    (document.head ?? document.documentElement).append(style);
  };
  if (document.documentElement) {
    install();
  } else {
    document.addEventListener("DOMContentLoaded", install, { once: true });
  }
}

// Runs in the page before Storybook's own scripts. It catches the preview's
// channel as Storybook installs it and records how the story ends.
function recordStoryOutcome() {
  // Init scripts also run in frames a story renders; only the preview counts.
  if (globalThis !== globalThis.top) return;
  // Stories persist UI state in web storage. Start each one from empty
  // storage, so no story sees another's.
  try {
    localStorage.clear();
    sessionStorage.clear();
  } catch {}
  const outcome = { done: false };
  globalThis.__tidebreakA11y = outcome;
  const finish = (result) => {
    if (!outcome.done) Object.assign(outcome, result, { done: true });
  };
  // Testing Library errors append a DOM dump; the first line says what failed.
  const message = (detail) =>
    String(detail?.message ?? detail?.description ?? detail?.title ?? detail ?? "unknown error")
      .split("\n")[0]
      .slice(0, 300);
  let channel;
  Object.defineProperty(globalThis, "__STORYBOOK_ADDONS_CHANNEL__", {
    configurable: true,
    get: () => channel,
    set(next) {
      channel = next;
      if (!next) return;
      for (const type of [
        "storyMissing",
        "storyErrored",
        "storyThrewException",
        "playFunctionThrewException",
      ]) {
        next.on(type, (detail) => finish({ failure: `${type}: ${message(detail)}` }));
      }
      next.on("storyFinished", ({ reporters }) => {
        const report = reporters?.find((entry) => entry.type === "a11y");
        if (!report) {
          finish({
            failure:
              "The story finished without an accessibility report. The a11y addon " +
              "did not run: check parameters.a11y and globals.a11y for this story.",
          });
        } else if (report.result?.error) {
          finish({ failure: `axe failed: ${message(report.result.error)}` });
        } else {
          finish({
            violations: report.result.violations.map((violation) => ({
              id: violation.id,
              impact: violation.impact,
              help: violation.help,
              helpUrl: violation.helpUrl,
              targets: violation.nodes.map((node) => node.target.flat().join(" ")),
            })),
          });
        }
      });
    },
  });
}

function describeViolation({ id, impact, help, helpUrl, targets }) {
  const shown = targets.slice(0, 3).join(", ");
  const more = targets.length > 3 ? ` (+${targets.length - 3} more)` : "";
  return `${impact} ${id}: ${help}\n  ${shown}${more}\n  ${helpUrl}`;
}

function matchesStory(pattern, id) {
  return pattern.endsWith("*") ? id.startsWith(pattern.slice(0, -1)) : id === pattern;
}

function checkAllowlistShape() {
  for (const entry of allowlist) {
    for (const field of ["story", "rule", "reason"]) {
      if (typeof entry[field] !== "string" || entry[field].trim() === "") {
        console.error(
          `Every ${allowlistFile} entry needs a non-empty ${field}: ${JSON.stringify(entry)}`,
        );
        process.exit(1);
      }
    }
  }
}

function serve(root) {
  const server = createServer(async (request, response) => {
    let file;
    try {
      const { pathname } = new URL(request.url, "http://127.0.0.1");
      file = normalize(join(root, decodeURIComponent(pathname)));
    } catch {
      response.writeHead(400).end();
      return;
    }
    if (!file.startsWith(root + sep)) {
      response.writeHead(403).end();
      return;
    }
    try {
      const body = await readFile(file);
      response.writeHead(200, {
        "cache-control": "max-age=3600",
        "content-type": MIME_TYPES[extname(file)] ?? "application/octet-stream",
      });
      response.end(body);
    } catch {
      response.writeHead(404).end();
    }
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}
