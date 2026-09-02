import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(
  process.env.TIDEBREAK_POLICY_ROOT ??
    fileURLToPath(new URL("..", import.meta.url)),
);
const repositoryFile = (...parts) => join(repositoryRoot, ...parts);
const workflowDirectory = repositoryFile(".github", "workflows");
const workflows = Object.fromEntries(
  readdirSync(workflowDirectory)
    .filter((name) => name.endsWith(".yml") || name.endsWith(".yaml"))
    .map((name) => [name, readFileSync(join(workflowDirectory, name), "utf8")]),
);
const releaseDrafterConfig = readFileSync(
  repositoryFile(".github", "release-drafter.yml"),
  "utf8",
);
const codeOwners = readFileSync(
  repositoryFile(".github", "CODEOWNERS"),
  "utf8",
);
const tauriConfig = JSON.parse(
  readFileSync(
    repositoryFile("crates", "tidebreak-desktop", "tauri.conf.json"),
    "utf8",
  ),
);
const desktopCargo = readFileSync(
  repositoryFile("crates", "tidebreak-desktop", "Cargo.toml"),
  "utf8",
);
const desktopHost = readFileSync(
  repositoryFile("crates", "tidebreak-desktop", "src", "lib.rs"),
  "utf8",
);
const desktopUpdater = readFileSync(
  repositoryFile("crates", "tidebreak-desktop", "src", "updater.rs"),
  "utf8",
);
const desktopBroker = readFileSync(
  repositoryFile("crates", "tidebreak-desktop", "src", "broker.rs"),
  "utf8",
);
const desktopVoice = readFileSync(
  repositoryFile(
    "crates",
    "tidebreak-desktop",
    "src",
    "voice_transcription.rs",
  ),
  "utf8",
);
const desktopWhisperInstall = readFileSync(
  repositoryFile(
    "crates",
    "tidebreak-desktop",
    "src",
    "whisper_install.rs",
  ),
  "utf8",
);
const dockerIgnore = readFileSync(
  repositoryFile("deploy", "self-host", "Dockerfile.dockerignore"),
  "utf8",
);
const selfHostDockerfile = readFileSync(
  repositoryFile("deploy", "self-host", "Dockerfile"),
  "utf8",
);
const denyConfig = readFileSync(repositoryFile("deny.toml"), "utf8");
const e2bPackage = JSON.parse(
  readFileSync(repositoryFile(".github", "e2b-cli", "package.json"), "utf8"),
);
const docsPackage = JSON.parse(
  readFileSync(repositoryFile("docs-site", "package.json"), "utf8"),
);
const vercelCliPackage = JSON.parse(
  readFileSync(repositoryFile(".github", "vercel-cli", "package.json"), "utf8"),
);
const tauriCliPackagePath = repositoryFile(
  ".github",
  "tauri-cli",
  "package.json",
);
const tauriCliLockPath = repositoryFile(
  ".github",
  "tauri-cli",
  "pnpm-lock.yaml",
);
const tauriCliPackage = existsSync(tauriCliPackagePath)
  ? JSON.parse(readFileSync(tauriCliPackagePath, "utf8"))
  : null;
const tauriCliLock = existsSync(tauriCliLockPath)
  ? readFileSync(tauriCliLockPath, "utf8")
  : null;
const packagedGhDiscoverySmoke = readFileSync(
  repositoryFile("scripts", "smoke-packaged-gh-discovery.sh"),
  "utf8",
);

function workflowJob(source, name) {
  const marker = `  ${name}:\n`;
  const start = source.indexOf(marker);
  assert.notEqual(start, -1, `missing workflow job: ${name}`);
  const remainder = source.slice(start + marker.length);
  const next = remainder.search(/^  [a-zA-Z0-9_-]+:\n/m);
  const end =
    next === -1 ? source.length : start + marker.length + next;
  return source.slice(start, end);
}

test("the Brightwave engineering team owns repository changes", () => {
  assert.equal(codeOwners.trim(), "* @brightwave-inc/engineering");
});

const DESKTOP_SIGNING_JOBS = [
  {
    file: "release.yml",
    name: "build_macos",
    validate: "Validate production signing configuration",
  },
  {
    file: "staging-publish.yml",
    name: "build_macos_staging",
    validate: "Validate staging signing configuration",
  },
  {
    file: "release.yml",
    name: "build_windows",
    validate: "Validate updater signing configuration",
  },
  {
    file: "release.yml",
    name: "build_linux",
    validate: "Verify and collect Linux artifacts",
  },
];

function desktopSigningJobs() {
  return DESKTOP_SIGNING_JOBS.map((spec) => ({
    ...spec,
    job: workflowJob(workflows[spec.file], spec.name),
  }));
}

function cargoDownloadCache(job) {
  return job.match(
    /- name: Cache Cargo downloads[\s\S]*?(?=\n\s+- (?:name:|uses:))/,
  )?.[0];
}

function firstSigningMaterialIndex(job, validate) {
  const markers = [
    `- name: ${validate}`,
    "- name: Prepare App Store Connect key",
    "- name: Import Developer ID certificate",
  ];
  const positions = markers
    .map((marker) => job.indexOf(marker))
    .filter((index) => index !== -1);
  return positions.length === 0 ? -1 : Math.min(...positions);
}

function assertCachesRestoreBeforeSigningMaterial(job, name, validate) {
  const secretsAt = firstSigningMaterialIndex(job, validate);
  assert.notEqual(secretsAt, -1, `${name} must load signing material`);
  const restores = job.matchAll(/^\s+- name: (.*Restore.*cache.*)$/gim);
  for (const restore of restores) {
    assert.ok(
      restore.index < secretsAt,
      `${name} must restore ${restore[1]} before loading secrets`,
    );
  }
}

function stripRustCommentsAndStrings(source) {
  const masked = [...source];
  const erase = (start, end) => {
    for (let index = start; index < end; index += 1) {
      if (masked[index] !== "\n") masked[index] = " ";
    }
  };
  for (let index = 0; index < source.length; index += 1) {
    if (source.startsWith("//", index)) {
      const end = source.indexOf("\n", index);
      erase(index, end === -1 ? source.length : end);
      index = end === -1 ? source.length : end;
    } else if (source.startsWith("/*", index)) {
      const end = source.indexOf("*/", index + 2);
      erase(index, end === -1 ? source.length : end + 2);
      index = end === -1 ? source.length : end + 1;
    } else if (source[index] === '"' || source[index] === "'") {
      const quote = source[index];
      let end = index + 1;
      while (end < source.length) {
        if (source[end] === "\\") end += 2;
        else if (source[end++] === quote) break;
      }
      erase(index, end);
      index = end - 1;
    }
  }
  return masked.join("");
}

function rustDelimitedBody(source, marker) {
  const stripped = stripRustCommentsAndStrings(source);
  const match = stripped.match(marker);
  assert.ok(match, `missing Rust body for ${marker}`);
  const start = match.index + match[0].length;
  const open = stripped.indexOf("{", start);
  assert.notEqual(open, -1, `missing opening brace for ${marker}`);
  let depth = 1;
  for (let index = open + 1; index < stripped.length; index += 1) {
    if (stripped[index] === "{") depth += 1;
    if (stripped[index] === "}" && --depth === 0) {
      return stripped.slice(open + 1, index);
    }
  }
  throw new Error(`unbalanced Rust body for ${marker}`);
}

function rustFunctionBody(source, name) {
  return rustDelimitedBody(source, new RegExp(`\\b(?:async\\s+)?fn\\s+${name}\\b`));
}

function hasOrderedUniqueTokens(body, tokens) {
  let previous = -1;
  return tokens.every((token) => {
    const position = body.indexOf(token);
    if (position === -1 || position <= previous || body.indexOf(token, position + token.length) !== -1) {
      return false;
    }
    previous = position;
    return true;
  });
}

function updaterTransitionIsSafe(updater, broker) {
  const restart = rustFunctionBody(updater, "take_staged_and_restart");
  const reversibleMarker = /\b(?:async\s+)?fn\s+install_behind_broker_barrier\b/;
  if (!reversibleMarker.test(stripRustCommentsAndStrings(updater))) {
    return false;
  }
  const barrier = rustFunctionBody(updater, "install_behind_broker_barrier");
  const quiesce = "quiesce().await?";
  const installCall = "match install()";
  const resume = "resume().await";
  const shutdown = "shutdown().await";
  const barrierOrdered =
    hasOrderedUniqueTokens(barrier, [quiesce, installCall, shutdown, resume]) &&
    (barrier.match(/\binstall\(\)/g) ?? []).length === 1;
  const bindings = /install_behind_broker_barrier\(\s*\|\| host_access\.quiesce_for_update\(\),\s*\|\| staged\.update\.install\(&staged\.bytes\),\s*\|\| host_access\.resume_after_failed_update\(\),\s*\|\| host_access\.shutdown\(\),\s*\)/s;
  const admit = rustFunctionBody(broker, "admit");
  const quiesceArm = rustDelimitedBody(broker, /BrokerCommand::Quiesce\s*\{\s*reply\s*\}\s*=>/);
  const resumeArm = rustDelimitedBody(
    broker,
    /BrokerCommand::ResumeAfterFailedUpdate\s*\{\s*reply\s*\}\s*=>/,
  );
  const session = rustFunctionBody(broker, "ensure_session");
  return (
    barrierOrdered &&
    bindings.test(restart) &&
    !admit.includes("drop(admission)") &&
    hasOrderedUniqueTokens(admit, [
      "BrokerAdmission::Running",
      "self.commands.try_send(command)",
    ]) &&
    hasOrderedUniqueTokens(quiesceArm, ["self.ensure_session().await", "reply.send("]) &&
    hasOrderedUniqueTokens(resumeArm, ["self.allow_session_start = false", "reply.send("]) &&
    hasOrderedUniqueTokens(session, [
      "if !self.allow_session_start",
      "return Err(BrokerClientError::UpdateRecovery)",
    ])
  );
}

test("third-party workflow actions use immutable commit SHAs", () => {
  for (const [name, source] of Object.entries(workflows)) {
    for (const match of source.matchAll(/^\s*(?:-\s*)?uses:\s*([^\s#]+)/gm)) {
      const reference = match[1];
      if (reference.startsWith("./")) {
        continue;
      }
      assert.match(
        reference,
        /^[^@\s]+@[0-9a-f]{40}$/,
        `${name} has a mutable action reference: ${reference}`,
      );
    }
  }
});

test("release-drafter retains a stable draft tag after formatting", () => {
  assert.match(releaseDrafterConfig, /name-template: "v\$RESOLVED_VERSION"/);
  assert.match(releaseDrafterConfig, /tag-template: "v\$RESOLVED_VERSION"/);
  assert.match(releaseDrafterConfig, /^tag-prefix: "v"$/m);
  assert.match(releaseDrafterConfig, /\$NEW_CONTRIBUTORS/);
  assert.match(releaseDrafterConfig, /\*\*Full Changelog\*\*:/);

  const draftJob = workflowJob(workflows["release-draft.yml"], "draft");
  const requireBaselineAt = draftJob.indexOf(
    "node scripts/require-release-baseline.mjs",
  );
  const releaseDrafterAt = draftJob.indexOf("id: release_drafter");
  assert.notEqual(requireBaselineAt, -1);
  assert.notEqual(releaseDrafterAt, -1);
  assert.ok(
    requireBaselineAt < releaseDrafterAt,
    "the published baseline must be confirmed before Release Drafter runs",
  );
  assert.match(draftJob, /git ls-remote --tags origin 'v\*'/);
  assert.match(
    draftJob,
    /RELEASE_TAG: v\$\{\{ steps\.release_drafter\.outputs\.resolved_version \}\}/,
  );
  assert.match(
    draftJob,
    /\{tag_name: \$tag, body: \$body\}/,
  );
  assert.match(draftJob, /node scripts\/reconcile-release-drafts\.mjs/);
  assert.match(draftJob, /Keep exactly one native release draft/);
  assert.match(draftJob, /max_attempts=5/);
  assert.match(draftJob, /\[\[ "\$action" != retry \]\]/);
  assert.match(draftJob, /\.delete_ids\[\]/);
  assert.match(draftJob, /jq -r \.action/);
});

test("publishing a release dispatches the server image build", () => {
  // A release this workflow publishes raises no `release` event, because the
  // PATCH that publishes it runs on GITHUB_TOKEN. Relying on the declared
  // `release` trigger alone shipped a release with no server image and no
  // failed run to notice. The dispatch is the working path; assert it stays.
  const finalizeJob = workflowJob(workflows["release.yml"], "finalize_release");
  assert.match(finalizeJob, /actions: write/);
  const dispatchAt = finalizeJob.indexOf(
    "gh workflow run publish-server-image.yml",
  );
  assert.notEqual(
    dispatchAt,
    -1,
    "finalize_release must dispatch the server image build",
  );
  assert.match(
    finalizeJob.slice(dispatchAt, dispatchAt + 200),
    /--field "release_tag=\$RELEASE_TAG"/,
  );

  // Dispatch only on the draft-to-published transition. Re-running the release
  // workflow against an already-published release must not rebuild an image,
  // because a version tag that exists on GHCR fails the publish.
  const dispatchStep = finalizeJob.slice(
    finalizeJob.lastIndexOf("- name:", dispatchAt),
    dispatchAt,
  );
  assert.match(
    dispatchStep,
    /if: \$\{\{ needs\.validate\.outputs\.draft == 'true' \}\}/,
  );

  // The declared trigger stays for a release published by hand in the UI.
  assert.match(
    workflows["publish-server-image.yml"],
    /^on:\n {2}release:\n {4}types: \[published\]$/m,
  );
});

test("workflow container images are pinned by digest", () => {
  for (const [name, source] of Object.entries(workflows)) {
    for (const match of source.matchAll(/^\s*image:\s*([^\s#]+)/gm)) {
      assert.match(
        match[1],
        /^[^@\s]+@sha256:[0-9a-f]{64}$/,
        `${name} has a mutable container image: ${match[1]}`,
      );
    }
  }

  assert.match(
    workflows["ci.yml"],
    /ghcr\.io\/gitleaks\/gitleaks@sha256:[0-9a-f]{64}/,
  );
  assert.doesNotMatch(workflows["ci.yml"], /gitleaks\/gitleaks:latest/);
});

test("PR lanes are scope-gated, never label-gated", () => {
  const ci = workflows["ci.yml"];
  const changes = workflowJob(ci, "changes");
  const docsSite = workflowJob(ci, "docs-site");
  const e2bCli = workflowJob(ci, "e2b-cli");
  const fmt = workflowJob(ci, "fmt");
  const postgres = workflowJob(ci, "postgres");
  const testJob = workflowJob(ci, "test");

  assert.match(
    ci,
    /^on:\n  push:\n    branches: \[main\]\n  pull_request:/m,
  );
  assert.match(
    ci,
    /pull_request:\n\s+types:\n\s+\[[^\]]*labeled, unlabeled[^\]]*\]/,
  );
  assert.doesNotMatch(ci, /^  build:$/m);
  assert.match(
    ci,
    /cp "\$trusted" "\$GITHUB_WORKSPACE\/scripts\/\.trusted-workflow-security\.test\.mjs"/,
  );
  assert.doesNotMatch(
    ci,
    /sed -i/,
    "the trusted policy runs as checked in; do not rewrite it",
  );
  assert.match(ci, /CANONICAL_REPOSITORY: brightwave-inc\/tidebreak/);
  assert.match(
    ci,
    /repos\/\$CANONICAL_REPOSITORY\/pulls\/\$PR_NUMBER/,
  );
  const releaseDraft = workflows["release-draft.yml"];
  assert.match(
    releaseDraft,
    /CANONICAL_REPOSITORY: brightwave-inc\/tidebreak/,
  );
  assert.match(
    workflowJob(releaseDraft, "label"),
    /repos\/\$CANONICAL_REPOSITORY\/issues\/\$PR_NUMBER/,
  );
  assert.match(
    ci,
    /node --test scripts\/\.trusted-workflow-security\.test\.mjs/,
  );
  assert.match(ci, /node --test scripts\/\*\.test\.mjs/);
  assert.doesNotMatch(
    ci,
    /cp "\$trusted" "\$GITHUB_WORKSPACE\/scripts\/workflow-security\.test\.mjs"/,
  );
  // A pull request's green checks must prove the same commits stay green on
  // main: no platform-neutral lane may hide behind an opt-in label. Native
  // Windows tests stay behind `windows-ci` / the windows scope; Windows
  // `cargo check` is rust-scoped like clippy.
  assert.doesNotMatch(ci, /full-ci/);
  assert.doesNotMatch(fmt, /github\.event_name/);
  assert.match(postgres, /TIDEBREAK_REQUIRE_POSTGRES_TEST: "true"/);
  // The narrower `workspace` scope must imply the `rust` one. Without this the
  // crate-coverage lanes could be gated on a scope that never ran for them.
  assert.match(
    changes,
    /if \[\[ "\$workspace" == true && "\$rust" != true \]\]; then/,
  );
  assert.doesNotMatch(ci, /^  rust:$/m);
  assert.doesNotMatch(ci, /name: fmt · clippy · build · test/);

  for (const [job, scope] of [
    [workflowJob(ci, "lint"), "rust"],
    [workflowJob(ci, "desktop"), "rust"],
    [workflowJob(ci, "windows-check"), "rust"],
    [testJob, "workspace"],
    [postgres, "workspace"],
    [workflowJob(ci, "ui"), "ui"],
    [docsSite, "docs_site"],
    [e2bCli, "e2b_cli"],
  ]) {
    assert.match(
      job,
      new RegExp(
        `if: \\$\\{\\{ needs\\.changes\\.outputs\\.${scope} == 'true' \\}\\}`,
      ),
    );
  }

  for (const job of [
    workflowJob(ci, "lint"),
    workflowJob(ci, "desktop"),
    testJob,
    postgres,
  ]) {
    assert.match(
      job,
      /shared-key: cargo-registry-v3-\$\{\{ hashFiles\('Cargo\.lock'\) \}\}/,
    );
    assert.match(job, /add-rust-environment-hash-key: "false"/);
    assert.match(job, /cache-targets: false/);
  }
  assert.match(testJob, /save-if: \$\{\{ github\.ref == 'refs\/heads\/main' \}\}/);
  assert.match(testJob, /cache-on-failure: true/);
  assert.match(
    testJob,
    /cargo nextest run --workspace --exclude tidebreak-desktop\n\s+--locked --retries 2/,
  );
  assert.doesNotMatch(ci, /^  parsers:$/m);
  assert.doesNotMatch(ci, /outputs\.parsers|echo "parsers=/);
  assert.match(changes, /\*\.md\|docs\/\*\|assets\/\*\|\.githooks\/\*/);

  // Documentation and publication tooling each have a pre-merge execution
  // lane. A tooling-only Dependabot update must not fall through to unrelated
  // Rust jobs, and edits to either workflow force its own lane.
  assert.match(changes, /docs-site\/\*\) docs_site=true/);
  assert.match(
    changes,
    /\.github\/e2b-cli\/\*\|\.github\/workflows\/publish-e2b-template\.yml\)\n\s+e2b_cli=true/,
  );
  assert.match(changes, /echo "docs_site=true"/);
  assert.match(changes, /echo "e2b_cli=true"/);
  assert.match(docsSite, /pnpm install --frozen-lockfile/);
  assert.match(docsSite, /run: pnpm types:check/);
  assert.match(docsSite, /run: pnpm lint/);
  assert.match(docsSite, /run: pnpm build/);

  assert.equal(e2bPackage.packageManager, "pnpm@10.18.3");
  assert.match(e2bCli, /version: 10\.18\.3/);
  assert.match(e2bCli, /node-version: 22/);
  assert.match(
    e2bCli,
    /pnpm --dir \.github\/e2b-cli install --frozen-lockfile --ignore-scripts/,
  );
  assert.match(
    e2bCli,
    /\.github\/e2b-cli\/node_modules\/\.bin\/e2b --version/,
  );
  assert.match(
    e2bCli,
    /dependencies\['@e2b\/cli'\]/,
  );
  assert.doesNotMatch(e2bCli, /npm install|@latest/);

  const desktop = workflowJob(ci, "desktop");
  assert.match(
    desktop,
    /cargo test -p tidebreak-desktop --locked/,
  );
  for (const [name, step] of [
    ["lint", "Install system deps (Tauri)"],
    ["desktop", "Install system deps (Tauri)"],
    ["test", "Install headless system deps"],
  ]) {
    const job = workflowJob(ci, name);
    const escapedStep = step.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const deps = job.match(
      new RegExp(
        `- name: ${escapedStep}[\\s\\S]*?(?=\\n\\s+- (?:name:|uses:))`,
      ),
    )?.[0];
    assert.ok(deps, `missing ${name} apt step`);
    assert.match(deps, /timeout-minutes: 8/);
    assert.match(deps, /scripts\/install-linux-apt-packages\.sh/);
    assert.doesNotMatch(deps, /sudo apt-get update/);
  }
  assert.match(
    desktopCargo,
    /tidebreak-server = \{ path = "\.\.\/tidebreak-server" \}/,
  );
  assert.doesNotMatch(desktopCargo, /document-parsers/);
});

test("merge queue groups re-run required CI", () => {
  const ci = workflows["ci.yml"];
  const changes = workflowJob(ci, "changes");
  const prTitle = workflowJob(ci, "pr-title");

  // Required checks never run unless the workflow listens for merge_group.
  // Keep the trigger after pull_request so the trusted base-branch trigger
  // regex still matches this file.
  assert.match(ci, /pull_request:\n\s+types:\n[\s\S]*?\n  merge_group:\n/);
  assert.match(changes, /merge_group\)\n\s+base_sha="\$MERGE_GROUP_BASE_SHA"/);
  assert.match(changes, /MERGE_GROUP_BASE_SHA: \$\{\{ github\.event\.merge_group\.base_sha \}\}/);
  assert.match(changes, /MERGE_GROUP_HEAD_SHA: \$\{\{ github\.event\.merge_group\.head_sha \}\}/);
  // The title job is a required check. Merge groups have no PR payload, so
  // the same name must still report success or the queue stalls.
  assert.match(
    prTitle,
    /if: \$\{\{ github\.event_name == 'pull_request' \|\| github\.event_name == 'merge_group' \}\}/,
  );
  assert.match(prTitle, /github\.event_name == 'merge_group'/);
});

// A main push that a newer commit has already replaced must not keep an
// expensive lane busy, and must not report `cancelled` when it stops: GitHub
// rolls a cancelled check up as a red X on a commit whose own checks all
// passed, so main's commit list reads as broken when it is not. A guard step
// reads main's tip and skips the run, which reports success.
//
// The shape only holds if every step the guard exists to avoid is gated on it.
// One ungated expensive step and a superseded run does its work anyway.
function skipsSupersededPush(job) {
  const guard =
    /^ {6}- name: [^\n]*\n {8}id: tip\n {8}if: \$\{\{ github\.event_name == 'push' \}\}\n/m.exec(
      job,
    );
  if (!guard) return false;
  if (!/repos\/\$GITHUB_REPOSITORY\/commits\/main/.test(job)) return false;
  if (!/superseded=true/.test(job)) return false;
  const rest = job.slice(guard.index + guard[0].length);
  const steps = rest.split(/\n(?= {6}- )/).slice(1);
  return (
    steps.length > 0 &&
    steps.every((step) =>
      /\n {8}if: \$\{\{ steps\.tip\.outputs\.superseded != 'true'/.test(step),
    )
  );
}

test("native Windows CI is scope-triggered, label-overridable, with a main backstop", () => {
  const ci = workflows["ci.yml"];
  const windows = workflowJob(ci, "windows-check");
  const changes = workflowJob(ci, "changes");
  const nativeTests = windows.match(
    /- name: Record native Windows test gate[\s\S]*?(?=\n {6}- name: Host broker Windows tests)/,
  )?.[0];
  assert.ok(nativeTests, "missing native Windows test gate step");
  assert.match(
    nativeTests,
    /github\.event_name != 'pull_request' \|\| contains\(github\.event\.pull_request\.labels\.\*\.name, 'windows-ci'\) \|\| needs\.changes\.outputs\.windows == 'true'/,
  );
  assert.match(nativeTests, /"\$EVENT_NAME" == merge_group/);
  assert.match(windows, /NATIVE_RUN: \$\{\{ steps\.native\.outputs\.run \}\}/);
  assert.match(windows, /Skipping native Windows tests\./);
  assert.match(windows, /vars\.CI_WINDOWS_RUNNER \|\| 'windows-latest'/);
  assert.match(windows, /CARGO_PROFILE_TEST_DEBUG: 0/);
  assert.match(windows, /-p tidebreak-cli/);
  assert.match(windows, /Stage sidecar placeholders for cargo check/);
  assert.doesNotMatch(windows, /prepare-sidecar\.mjs/);
  assert.match(changes, /windows: \$\{\{ steps\.scope\.outputs\.windows \}\}/);
  assert.match(changes, /echo "windows=\$windows"/);
  assert.match(changes, /echo "windows=true"/);
  // The scope must imply the Rust one, for the same reason `workspace` does:
  // the lane is gated on both, so a scope the Rust gate never admits is dead.
  assert.match(
    changes,
    /if \[\[ "\$windows" == true && "\$rust" != true \]\]; then/,
  );
  // Every crate or module the lane runs tests for. Dropping one silently
  // returns that boundary to label-only coverage.
  for (const boundary of [
    "crates/tidebreak-code-execution/\\*",
    "crates/tidebreak-harness/\\*",
    "crates/tidebreak-host-broker/\\*",
    "crates/tidebreak-server/src/code/\\*",
    "crates/tidebreak-server/src/tests/code\\*",
    "crates/tidebreak-server/src/desktop_schema\\.rs",
    "crates/tidebreak-core/src/keychain\\.rs",
    "crates/tidebreak-desktop/scripts/prepare-sidecar\\.mjs",
  ]) {
    assert.match(
      changes,
      new RegExp(`${boundary}[^\\n]*\\n?[^\\n]*windows=true`),
      `${boundary} must set the Windows scope`,
    );
  }
  assert.ok(
    skipsSupersededPush(windows),
    "a superseded main push must skip the Windows lane, not cancel it",
  );
  assert.doesNotMatch(
    windows,
    /cancel-in-progress/,
    "cancelling this lane reddens a commit whose own checks all passed",
  );
  assert.match(windows, /SCCACHE_GHA_ENABLED: "true"/);
  assert.match(windows, /SCCACHE_GHA_RW_MODE: READ_WRITE/);
  assert.match(windows, /RUSTC_WRAPPER: sccache/);
  assert.match(
    windows,
    /uses: mozilla-actions\/sccache-action@[0-9a-f]{40}/,
  );
  assert.match(windows, /\$attempts = 3/);
  assert.match(windows, /tidebreak-server", "--lib", "code::"/);
  assert.match(windows, /failed on attempt/);
  assert.match(
    windows,
    /cargo check --target x86_64-pc-windows-msvc/,
  );
  assert.match(
    windows,
    /cargo test --target x86_64-pc-windows-msvc -p tidebreak-host-broker --locked/,
  );
});

test("UI tests and production build each gate the UI lane", () => {
  const ci = workflows["ci.yml"];
  const ui = workflowJob(ci, "ui");

  assert.match(ui, /if:.*needs\.changes\.outputs\.ui == 'true'/);
  assert.match(ui, /run: pnpm install --frozen-lockfile/);
  // Sequential steps, one command each: a backgrounded `a & b & wait` swallows
  // the children's exit codes, so a failing test or build reported success
  // (#1376). Each step's status must reach the job directly.
  assert.match(ui, /run: pnpm exec biome format src/);
  assert.match(ui, /run: pnpm lint/);
  assert.match(ui, /run: pnpm test/);
  assert.match(ui, /run: pnpm build/);
  assert.doesNotMatch(ui, /& wait/);
  assert.doesNotMatch(ci, /matrix\.task/);
});

test("PR compiler caches are writable, isolated, and deleted on close", () => {
  const ci = workflows["ci.yml"];
  for (const name of [
    "lint",
    "desktop",
    "windows-check",
    "test",
    "postgres",
  ]) {
    assert.match(workflowJob(ci, name), /SCCACHE_GHA_RW_MODE: READ_WRITE/);
  }
  assert.doesNotMatch(
    ci,
    /github\.event_name == 'pull_request' && 'READ_ONLY' \|\| 'READ_WRITE'/,
  );

  const cleanup = workflows["cache-cleanup.yml"];
  assert.ok(cleanup);
  assert.match(
    cleanup,
    /^on:\n(?:  #.*\n)+  pull_request_target:\n    types: \[closed\]/m,
  );
  assert.match(cleanup, /^permissions:\n  actions: write\n  contents: read$/m);
  assert.match(
    cleanup,
    /gh cache delete --repo "\$GITHUB_REPOSITORY"\n\s+--all --succeed-on-no-caches\n\s+--ref "refs\/pull\/\$\{PR_NUMBER\}\/merge"/,
  );
  assert.doesNotMatch(cleanup, /actions\/checkout|secrets\./);
});

test("production secrets remain isolated to the release workflow", () => {
  const secretConsumers = Object.entries(workflows)
    .filter(([, source]) => source.includes("secrets."))
    .map(([name]) => name);
  const allowedSecretConsumers = new Set([
    "publish-e2b-template.yml",
    "publish-whisper-helper.yml",
    "release-draft.yml",
    "release.yml",
    "staging-publish.yml",
  ]);
  for (const name of secretConsumers) {
    assert.ok(
      allowedSecretConsumers.has(name),
      `unexpected secret consumer: ${name}`,
    );
  }

  // The release draft may read exactly one secret: the private key of the
  // GitHub App whose token replaces GITHUB_TOKEN for the draft job, so the
  // release API calls stop sharing the repository's Actions rate budget. The
  // key stays out of the label and backfill jobs: the label job runs on
  // pull_request_target events, and neither job needs more than GITHUB_TOKEN.
  const releaseDraftSource = workflows["release-draft.yml"];
  for (const secret of releaseDraftSource.match(/secrets\.[A-Za-z0-9_]+/g) ?? []) {
    assert.equal(
      secret,
      "secrets.RELEASE_APP_PRIVATE_KEY",
      `release-draft.yml may read only the app private key, found ${secret}`,
    );
  }
  for (const job of ["label", "backfill"]) {
    assert.doesNotMatch(
      workflowJob(releaseDraftSource, job),
      /secrets\./,
      `the ${job} job must stay on GITHUB_TOKEN`,
    );
  }
  assert.ok(secretConsumers.includes("publish-e2b-template.yml"));
  assert.ok(secretConsumers.includes("release.yml"));

  const release = workflows["release.yml"];
  assert.doesNotMatch(release, /^  release:/m);
  assert.match(release, /^  workflow_dispatch:\n/m);
  assert.doesNotMatch(release, /^\s*pull_request(?:_target)?:/m);
  assert.match(release, /^permissions:\n  contents: read$/m);

  // The whisper helper publish is allowed the Tauri updater signing key,
  // and only because it can never run from a pull request: it is
  // manual-dispatch only, uses the desktop-production environment, and its
  // signed artifacts are verified by the desktop against the committed
  // updater public key before they can run. If the workflow doesn't exist
  // yet, these assertions are vacuously true.
  const whisper = workflows["publish-whisper-helper.yml"];
  if (whisper) {
    assert.match(whisper, /^on:\n  workflow_dispatch:\n/m);
    assert.doesNotMatch(whisper, /^\s*pull_request(?:_target)?:/m);
    assert.match(whisper, /^permissions:\n  contents: read$/m);
    assert.match(whisper, /cancel-in-progress: false/);
    assert.match(whisper, /environment:\n\s+name: desktop-production/);
    assert.deepEqual(
      [...new Set(whisper.match(/secrets\.[A-Z0-9_]+/g))].sort(),
      ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"]
        .map((name) => `secrets.${name}`),
    );
    const whisperPublish = workflowJob(whisper, "publish");
    assert.match(whisperPublish, /tauri signer sign "\$file"/);
    assert.doesNotMatch(whisperPublish, /cargo tauri signer sign/);
  }

  // The E2B template publish is the one other workflow allowed a secret, and
  // only because it can never run from a pull request: it triggers on pushes
  // to main touching the template definition, plus manual dispatch. Its
  // credential is scoped to E2B — it must not reach the signing secrets.
  const e2b = workflows["publish-e2b-template.yml"];
  const resolveJob = workflowJob(e2b, "resolve");
  const publishJob = workflowJob(e2b, "publish");
  assert.match(e2b, /^on:\n  push:\n    branches: \[main\]\n    paths:\n/m);
  assert.match(e2b, /^      - "\.github\/e2b-cli\/\*\*"$/m);
  assert.match(e2b, /^  workflow_dispatch:\n/m);
  assert.match(
    e2b,
    /^      release_tag:\n(?:        .*\n)+?        type: string$/m,
  );
  assert.doesNotMatch(e2b, /^\s*pull_request(?:_target)?:/m);
  assert.match(e2b, /^permissions:\n  contents: read$/m);
  assert.deepEqual(
    [...new Set(e2b.match(/secrets\.[A-Z0-9_]+/g))].sort(),
    // Assembled rather than written out: a literal list of quoted
    // secret-shaped names reads as a credential to the secret scanner.
    ["ACCESS_TOKEN", "API_KEY"].map((suffix) => `secrets.E2B_${suffix}`),
  );

  // A manual run must execute the current default-branch workflow. A release
  // tag is validated as input and resolved to a commit for a sparse source-only
  // checkout; it can never supply the workflow or locked CLI definition.
  assert.match(resolveJob, /DEFAULT_BRANCH: \$\{\{ github\.event\.repository\.default_branch \}\}/);
  assert.equal(
    resolveJob.match(
      /SOURCE_REF" == "refs\/heads\/\$DEFAULT_BRANCH" &&\n\s+"\$SOURCE_REF_NAME" == "\$DEFAULT_BRANCH"/g,
    )?.length,
    2,
    "both push and workflow_dispatch must require the default-branch ref",
  );
  assert.doesNotMatch(resolveJob, /SOURCE_REF" == "refs\/tags\//);
  assert.match(resolveJob, /RELEASE_TAG: \$\{\{ inputs\.release_tag \}\}/);
  assert.match(resolveJob, /node scripts\/check-release-tag\.mjs "\$RELEASE_TAG"/);
  assert.match(
    resolveJob,
    /repos\/\$GITHUB_REPOSITORY\/releases\/tags\/\$RELEASE_TAG/,
  );
  assert.match(resolveJob, /refs\/tags\/\$RELEASE_TAG\^\{commit\}/);
  assert.match(
    resolveJob,
    /git merge-base --is-ancestor "\$template_sha" "origin\/\$DEFAULT_BRANCH"/,
  );
  assert.match(resolveJob, /echo "sha=\$template_sha" >> "\$GITHUB_OUTPUT"/);
  assert.match(resolveJob, /ref: \$\{\{ steps\.source\.outputs\.sha \}\}/);
  assert.match(
    resolveJob,
    /sparse-checkout: \|\n\s+crates\/tidebreak-sandbox-agent\/e2b/,
  );
  assert.match(publishJob, /ref: \$\{\{ github\.sha \}\}/);
  assert.match(
    publishJob,
    /ref: \$\{\{ needs\.resolve\.outputs\.source_sha \}\}/,
  );
  assert.match(
    publishJob,
    /working-directory: \$\{\{ env\.SOURCE_TEMPLATE_DIR \}\}/,
  );

  // Source validation and all dependency setup happen before any secret-bearing
  // step. Credentials remain step-scoped to the API calls that require them.
  assert.ok(
    resolveJob.indexOf("Validate the publication source") <
      resolveJob.indexOf("Require the E2B credential"),
  );
  assert.ok(
    resolveJob.indexOf("Check out the validated template source") <
      resolveJob.indexOf("Require the E2B credential"),
  );
  const sourceValidationStep = resolveJob.match(
    /- name: Validate the publication source[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const sourceCheckoutStep = resolveJob.match(
    /- name: Check out the validated template source[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(sourceValidationStep);
  assert.ok(sourceCheckoutStep);
  assert.doesNotMatch(sourceValidationStep, /secrets\./);
  assert.doesNotMatch(sourceCheckoutStep, /secrets\./);
  assert.doesNotMatch(
    publishJob.match(/^    env:\n[\s\S]*?(?=^    steps:)/m)?.[0] ?? "",
    /secrets\./,
  );
  assert.ok(
    publishJob.indexOf("Install the locked E2B CLI") <
      publishJob.indexOf("secrets."),
  );
  assert.match(
    publishJob,
    /pnpm --dir \.github\/e2b-cli install --frozen-lockfile --ignore-scripts/,
  );
  assert.match(
    publishJob,
    /\.github\/e2b-cli\/node_modules\/\.bin\/e2b --version/,
  );
  assert.doesNotMatch(publishJob, /npm install|@latest/);
});

test("desktop voice delegates whisper.cpp to the verified helper", () => {
  assert.match(desktopHost, /^mod whisper_install;$/m);
  assert.match(
    desktopVoice,
    /crate::whisper_install::ensure_helper\(&self\.data_dir\)/,
  );
  assert.match(desktopVoice, /tokio::process::Command::new\(helper\)/);
  assert.doesNotMatch(desktopVoice, /\bWhisperContext\b|whisper_rs/);
  assert.doesNotMatch(desktopCargo, /^whisper-rs\s*=/m);
  assert.match(desktopCargo, /^minisign-verify\.workspace = true$/m);
  assert.match(
    desktopWhisperInstall,
    /sha256_hex_of_file\(&binary\).*marker\.binary_sha256/s,
  );

  const releaseWindows = workflowJob(workflows["release.yml"], "build_windows");
  const warmWindows = workflows["cache-windows.yml"];
  for (const job of [releaseWindows, warmWindows]) {
    assert.doesNotMatch(job, /Use clang-cl for Windows ARM native code/);
    assert.doesNotMatch(job, /CMAKE_GENERATOR=Ninja/);
  }

  const helperBuild = workflowJob(
    workflows["publish-whisper-helper.yml"],
    "build",
  );
  assert.match(helperBuild, /Use clang-cl for Windows ARM native code/);
  assert.match(helperBuild, /CMAKE_GENERATOR=Ninja/);
});

test("dependency policy covers advisories, licenses, sources, and the locked graph", () => {
  const advisories = workflowJob(workflows["ci.yml"], "advisories");
  assert.match(denyConfig, /^\[advisories\]$/m);
  assert.match(denyConfig, /^\[licenses\]$/m);
  assert.match(denyConfig, /^\[sources\]$/m);
  assert.match(
    advisories,
    /cargo deny --all-features --locked check advisories licenses sources/,
  );
});

test(
  "every workspace crate opts out of crates.io publication in its own manifest",
  // Mutation fixtures copy policy files, not the Cargo workspace.
  { skip: !existsSync(repositoryFile("Cargo.toml")) },
  () => {
    const metadata = JSON.parse(
      execFileSync(
        "cargo",
        ["metadata", "--format-version", "1", "--no-deps"],
        { cwd: repositoryRoot, encoding: "utf8" },
      ),
    );
    const workspaceMembers = new Set(metadata.workspace_members);
    const missing = metadata.packages
      .filter((pkg) => workspaceMembers.has(pkg.id))
      .filter((pkg) => {
        const manifest = readFileSync(pkg.manifest_path, "utf8");
        const packageStart = manifest.indexOf("[package]");
        if (packageStart === -1) return true;
        const sectionEnd = manifest.indexOf("\n[", packageStart + 1);
        const packageSection = manifest.slice(
          packageStart + "[package]".length,
          sectionEnd === -1 ? undefined : sectionEnd,
        );
        return !/^publish\s*=\s*false\s*$/m.test(packageSection);
      })
      .map((pkg) => pkg.manifest_path);

    assert.deepEqual(
      missing,
      [],
      "every workspace crate must set publish = false in its own [package] section",
    );
  },
);

test("E2B template pin provenance and writes share the validated source revision", () => {
  const pin = workflowJob(workflows["publish-e2b-template.yml"], "pin");

  assert.match(pin, /SOURCE_SHA: \$\{\{ needs\.resolve\.outputs\.source_sha \}\}/);
  assert.match(pin, /name: Check out the validated template provenance/);
  assert.match(pin, /ref: \$\{\{ needs\.resolve\.outputs\.source_sha \}\}/);
  assert.match(pin, /path: \.release-source/);
  assert.match(pin, /git diff --quiet --no-index "\.release-source\/\$TEMPLATE_PATH" "\$TEMPLATE_PATH"/);
  assert.match(pin, /directory = f"\.release-source\/\{os\.environ\['TEMPLATE_PATH'\]\}"/);
});

test("the self-host Docker context is allowlisted and denies hidden credentials", () => {
  assert.match(dockerIgnore, /^\*\*$/m);
  assert.doesNotMatch(dockerIgnore, /^!crates\/\*\*$/m);
  assert.doesNotMatch(dockerIgnore, /^!skills\/\*\*$/m);
  assert.doesNotMatch(dockerIgnore, /^!plugins\/\*\*$/m);

  for (const required of [
    "!Cargo.toml",
    "!Cargo.lock",
    "!rust-toolchain.toml",
    "!crates/*/Cargo.toml",
    "!crates/*/build.rs",
    "!crates/*/src/**",
    "!crates/tidebreak-code-execution/baseline_python_deps.txt",
    "!crates/tidebreak-sandbox-agent/documents-requirements.txt",
    "!skills/*/SKILL.md",
    "!plugins/*/PLUGIN.md",
    "!crates/*/src/**/*.rs",
  ]) {
    assert.ok(dockerIgnore.includes(`${required}\n`), `missing allow rule ${required}`);
  }

  for (const denied of [
    "**/.npmrc",
    "**/.netrc",
    "**/.pypirc",
    "**/.cargo/credentials",
    "**/.cargo/credentials.toml",
    "**/id_*",
    "**/credentials",
    "**/credentials.*",
    "**/.*",
    "**/.*/**",
  ]) {
    assert.ok(dockerIgnore.includes(`${denied}\n`), `missing deny rule ${denied}`);
  }
  assert.match(
    readFileSync(repositoryFile("scripts", "stage-self-host-build-context.sh"), "utf8"),
    /git -C "\$root" archive --format=tar "\$revision" \| tar -x -C "\$destination"/,
  );
});

test("the self-host runtime installs packages from an immutable Debian snapshot", () => {
  assert.match(
    selfHostDockerfile,
    /snapshot\.debian\.org\/archive\/debian\/[0-9]{8}T[0-9]{6}Z/,
  );
  assert.match(
    selfHostDockerfile,
    /snapshot\.debian\.org\/archive\/debian-security\/[0-9]{8}T[0-9]{6}Z/,
  );
  assert.match(selfHostDockerfile, /ca-certificates=[^\s\\]+/);
  assert.match(selfHostDockerfile, /curl=[^\s\\]+/);
  assert.doesNotMatch(
    selfHostDockerfile,
    /apt-get install -y --no-install-recommends ca-certificates curl/,
  );
});

test("the published server image states the release it carries", () => {
  // The workspace manifest stays at 0.0.0, so the release tag reaches the
  // binary only through this build argument. Without it `--version` answers
  // "0.0.0-unreleased" and the smoke test below catches the mismatch.
  assert.match(selfHostDockerfile, /^ARG TIDEBREAK_VERSION="0\.0\.0-unreleased"$/m);

  const buildJob = workflowJob(workflows["publish-server-image.yml"], "build");
  assert.match(buildJob, /--build-arg "TIDEBREAK_VERSION=\$VERSION"/);

  // Asserting the reported string, not just a zero exit. The smoke test that
  // shipped ran `--version` against a binary with no such flag; it would have
  // failed on any build, and did on the first one that compiled.
  assert.match(buildJob, /\[\[ "\$reported" = "tidebreak \$VERSION" \]\]/);
});

test(
  "BuildKit admits only exact source inputs from allowed source paths",
  { skip: process.env.TIDEBREAK_SKIP_DOCKER_CONTEXT_PROBE === "1" },
  () => {
    const context = mkdtempSync(join(tmpdir(), "tidebreak-docker-context-"));
    const output = mkdtempSync(join(tmpdir(), "tidebreak-docker-output-"));
    const write = (path, contents = "probe\n") => {
      const target = join(context, path);
      mkdirSync(dirname(target), { recursive: true });
      writeFileSync(target, contents);
    };

    const included = [
      "Cargo.toml",
      "Cargo.lock",
      "rust-toolchain.toml",
      "crates/demo/Cargo.toml",
      "crates/demo/build.rs",
      "crates/demo/src/lib.rs",
      // A source module named like a credential file must survive the deny
      // block: tidebreak-server has src/web_search/credentials.rs for real.
      "crates/demo/src/web/credentials.rs",
      "crates/tidebreak-code-execution/baseline_python_deps.txt",
      "crates/tidebreak-sandbox-agent/documents-requirements.txt",
      "skills/demo/SKILL.md",
      "plugins/demo/PLUGIN.md",
    ];
    const excluded = [
      "crates/demo/.npmrc",
      "crates/demo/src/.netrc",
      "crates/demo/src/deep/.pypirc",
      "crates/demo/src/id_rsa",
      "crates/demo/src/deep/id_ed25519",
      "crates/demo/src/.cargo/credentials",
      "crates/demo/src/.cargo/credentials.toml",
      "crates/demo/src/deep/.harmless-hidden-file",
      "skills/demo/.draft",
      "plugins/demo/credentials.json",
      "crates/demo/src/credentials.json",
      "outside/secret.txt",
    ];

    try {
      for (const path of [...included, ...excluded]) {
        write(path);
      }
      writeFileSync(
        join(context, "Dockerfile"),
        "FROM scratch\nCOPY . /context\n",
      );
      writeFileSync(join(context, "Dockerfile.dockerignore"), dockerIgnore);

      execFileSync(
        "docker",
        [
          "buildx",
          "build",
          "--file",
          join(context, "Dockerfile"),
          "--output",
          `type=local,dest=${output}`,
          context,
        ],
        { stdio: "pipe" },
      );

      for (const path of included) {
        assert.ok(existsSync(join(output, "context", path)), `missing ${path}`);
      }
      for (const path of excluded) {
        assert.ok(
          !existsSync(join(output, "context", path)),
          `credential probe escaped the context: ${path}`,
        );
      }
    } finally {
      rmSync(context, { recursive: true, force: true });
      rmSync(output, { recursive: true, force: true });
    }
  },
);

test(
  "the self-host build context excludes arbitrary untracked source files",
  { skip: process.env.TIDEBREAK_SKIP_DOCKER_CONTEXT_PROBE === "1" },
  () => {
    const context = mkdtempSync(join(tmpdir(), "tidebreak-self-host-context-"));
    const output = mkdtempSync(join(tmpdir(), "tidebreak-self-host-output-"));
    const probe = repositoryFile("crates", "cloud-token.txt");
    try {
      writeFileSync(probe, "untracked probe\n");
      execFileSync("bash", [repositoryFile("scripts", "stage-self-host-build-context.sh"), context]);
      writeFileSync(join(context, "Probe.Dockerfile"), "FROM scratch\nCOPY . /context\n");
      execFileSync(
        "docker",
        ["buildx", "build", "--file", join(context, "Probe.Dockerfile"), "--output", `type=local,dest=${output}`, context],
        { stdio: "pipe" },
      );
      assert.ok(existsSync(join(output, "context", "Cargo.toml")));
      assert.ok(!existsSync(join(output, "context", "crates", "cloud-token.txt")));
    } finally {
      rmSync(probe, { force: true });
      rmSync(context, { recursive: true, force: true });
      rmSync(output, { recursive: true, force: true });
    }
  },
);

function sandboxPublishControls(publish) {
  const resolve = workflowJob(publish, "resolve");
  const build = workflowJob(publish, "build");
  const dockerIgnorePath = repositoryFile(
    "crates",
    "tidebreak-sandbox-agent",
    "Dockerfile.dockerignore",
  );
  return {
    resolve,
    build,
    dockerIgnorePath,
    hasDefaultBranchEnv:
      /DEFAULT_BRANCH: \$\{\{ github\.event\.repository\.default_branch \}\}/.test(
        resolve,
      ),
    hasDefaultBranchRefGate:
      /SOURCE_REF" == "refs\/heads\/\$DEFAULT_BRANCH" &&\n\s+"\$SOURCE_REF_NAME" == "\$DEFAULT_BRANCH"/.test(
        resolve,
      ),
    hasAncestorGate:
      /git merge-base --is-ancestor "\$(?:GITHUB_SHA|SOURCE_SHA)" "origin\/\$DEFAULT_BRANCH"/.test(
        resolve,
      ),
    hasPersistCredentialsFalse: /persist-credentials:\s*false/.test(build),
    hasDockerIgnore: existsSync(dockerIgnorePath),
  };
}

function sandboxPublishTrigger(publish) {
  return {
    tagPush: /^on:\n  push:\n    tags: \["v\*"\]\n    branches: \[main\]\n  schedule:/m.test(
      publish,
    ),
    publishedRelease:
      /^on:\n  release:\n    types: \[published\]\n  push:\n    branches: \[main\]\n  schedule:/m.test(
        publish,
      ),
  };
}

function assertSandboxPublishReleaseEvent(publish) {
  const resolve = workflowJob(publish, "resolve");
  assert.match(
    publish,
    /^on:\n  release:\n    types: \[published\]\n  push:\n    branches: \[main\]\n  schedule:/m,
  );
  assert.match(resolve, /EVENT_NAME: \$\{\{ github\.event_name \}\}/);
  assert.match(resolve, /RELEASE_TAG: \$\{\{ github\.event\.release\.tag_name \}\}/);
  assert.match(resolve, /github\.event\.release|RELEASE_TAG/);
  assert.match(resolve, /prerelease/);
  assert.doesNotMatch(publish, /^\s*tags: \["v\*"\]/m);
}

function assertSandboxPublishMainOnly(controls) {
  assert.ok(
    controls.hasDefaultBranchEnv,
    "resolve must identify the repository default branch",
  );
  assert.ok(
    controls.hasDefaultBranchRefGate,
    "dispatch and schedule must require the default-branch workflow",
  );
  assert.match(
    controls.resolve,
    /workflow_dispatch\|schedule|schedule\|workflow_dispatch/,
  );
  assert.ok(
    controls.hasAncestorGate,
    "tag and dispatch must refuse SHAs not contained in the default branch",
  );
  assert.ok(
    controls.hasPersistCredentialsFalse,
    "build checkout must not persist GITHUB_TOKEN into .git/config",
  );
  assert.ok(
    controls.hasDockerIgnore,
    "sandbox image build must ship a Dockerfile.dockerignore",
  );
  const dockerIgnore = readFileSync(controls.dockerIgnorePath, "utf8");
  assert.match(dockerIgnore, /^\.git$/m);
  assert.match(dockerIgnore, /^\.git\/\*\*$/m);
}

test("sandbox image publishing is tag-driven, immutable, and secret-free", () => {
  const publish = workflows["publish-sandbox-image.yml"];
  assert.ok(publish);

  // A published GitHub Release runs this file from the default branch.
  // A `v*` tag push would run the tagged commit's YAML instead, so it is
  // not a trigger.
  assert.ok(sandboxPublishTrigger(publish).publishedRelease);
  assert.ok(!sandboxPublishTrigger(publish).tagPush);
  assertSandboxPublishReleaseEvent(publish);
  assert.match(publish, /cron: "43 4 \* \* 4"/);
  assert.match(publish, /^  workflow_dispatch:/m);
  assert.doesNotMatch(publish, /^\s*pull_request(?:_target)?:/m);

  // Dispatch/schedule must run the default-branch workflow. Every publish,
  // tag included, refuses a SHA that is not an ancestor of that branch.
  // The build checkout must not persist GITHUB_TOKEN, and `.git` stays out
  // of the Docker context.
  assertSandboxPublishMainOnly(sandboxPublishControls(publish));

  // A main push publishes only when the image inputs changed; the scope lives
  // in the resolve job (an `on.push.paths` filter would also gate the tag
  // trigger).
  const resolve = workflowJob(publish, "resolve");
  assert.match(
    resolve,
    /crates\/tidebreak-sandbox-agent\/\*\|scripts\/exec-documents\/\*\|\.github\/workflows\/publish-sandbox-image\.yml/,
  );

  // Non-release rebuilds mint tags that can never collide with a release tag
  // (they never start with `v`) and are unique per run; both schemes are
  // validated before anything builds.
  assert.match(resolve, /main-\$\(date -u \+%Y%m%d\)-\$\{GITHUB_SHA:0:7\}-r\$\{GITHUB_RUN_NUMBER\}/);
  assert.match(resolve, /\^main-\[0-9\]\{8\}-\[0-9a-f\]\{7\}-r\[0-9\]\+\$/);
  assert.match(resolve, /\^v\[0-9\]\+\\\.\[0-9\]\+\\\.\[0-9\]\+/);

  // The default token with packages:write is the whole credential surface —
  // publishing must not grow a dependency on repository secrets (that set
  // stays isolated to release.yml by the production-secrets test above).
  // Only the jobs that push (build) or assemble the version tag (manifest)
  // may hold packages:write; the scanner is packages:read.
  assert.match(publish, /^permissions:\n  contents: read$/m);
  assert.equal(publish.match(/packages: write/g)?.length, 2);
  assert.match(
    workflowJob(publish, "build"),
    /^    permissions:\n      contents: read\n      packages: write$/m,
  );
  assert.match(
    workflowJob(publish, "manifest"),
    /^    permissions:\n      contents: read\n      packages: write$/m,
  );
  assert.doesNotMatch(publish, /secrets\./);

  // Version tags are immutable: both the per-arch build job and the manifest
  // job refuse a tag that already exists instead of repointing it.
  const overwriteGuards = publish.match(
    /Refusing to overwrite existing image tag/g,
  );
  assert.equal(overwriteGuards?.length, 2);
  assert.match(publish, /docker manifest inspect "\$repository:\$IMAGE_TAG"/);

  // Published refs stay under this owner's Tidebreak GHCR namespace, and the
  // digest the local backend pins is surfaced by the run itself.
  assert.match(
    publish,
    /ghcr\.io\/\$\{\{ github\.repository_owner \}\}\/tidebreak-sandbox-agent$/m,
  );
  assert.match(
    publish,
    /ghcr\.io\/\$\{\{ github\.repository_owner \}\}\/tidebreak-sandbox-agent-documents$/m,
  );
  assert.match(publish, /PUBLISHED_IMAGE_DIGEST/);
});

test("sandbox images are scanned in a read-only job before the version tag is published, and the pin loop never touches main", () => {
  const publish = workflows["publish-sandbox-image.yml"];
  const build = workflowJob(publish, "build");
  const scan = workflowJob(publish, "scan");
  const manifest = workflowJob(publish, "manifest");
  const pin = workflowJob(publish, "pin");

  // The scanner is a checksum-pinned binary release, not a third-party
  // action, and it is the only job that runs trivy. It cannot publish:
  // packages:read, no checkout (so no persisted git credentials), and the
  // version-tag job waits for it.
  assert.doesNotMatch(build, /trivy/i);
  assert.doesNotMatch(manifest, /trivy/i);
  assert.match(scan, /trivy_\$\{TRIVY_VERSION\}_\$\{asset\}\.tar\.gz/);
  assert.match(scan, /sha256sum --check/);
  assert.match(publish, /^  TRIVY_VERSION: \d+\.\d+\.\d+$/m);
  assert.match(scan, /^    permissions:\n      contents: read\n      packages: read$/m);
  assert.doesNotMatch(scan, /packages: write/);
  assert.doesNotMatch(scan, /actions\/checkout/);
  assert.doesNotMatch(scan, /secrets\./);
  assert.match(manifest, /needs: \[resolve, build, scan\]/);

  // Policy: the run summary carries the full all-severities report, but the
  // publish fails only on fixable CRITICAL vulnerabilities or any secret. A
  // broader gate would drown in LibreOffice CVE noise and be ignored.
  assert.match(
    scan,
    /trivy image --timeout 15m --scanners vuln,secret \\\n\s+--format table --output "\$report" "\$image"/,
  );
  assert.match(
    scan,
    /trivy image --timeout 15m --scanners vuln \\\n\s+--severity CRITICAL --ignore-unfixed --exit-code 1 "\$image"/,
  );
  assert.match(
    scan,
    /trivy image --timeout 15m --scanners secret --exit-code 1 "\$image"/,
  );

  // The pin job proposes a PR from its automation branch with the workflow's
  // own token; it must never push to main, and the write scopes stay confined
  // to that job.
  assert.match(pin, /PIN_BRANCH: automation\/sandbox-image-pin/);
  assert.match(pin, /git push --force origin "HEAD:refs\/heads\/\$PIN_BRANCH"/);
  assert.doesNotMatch(pin, /git push[^\n]*(?:origin main|HEAD:main|refs\/heads\/main)/);
  assert.match(pin, /^    permissions:\n      contents: write\n      pull-requests: write\n      packages: read$/m);
  assert.equal(publish.match(/contents: write/g)?.length, 1);
});

test("release builds freeze a draft tag from the trusted main workflow", () => {
  const release = workflows["release.yml"];
  const validateJob = workflowJob(release, "validate");
  assert.doesNotMatch(release, /^  release:/m);
  assert.match(release, /^  workflow_dispatch:\n/m);
  assert.match(validateJob, /github\.ref == 'refs\/heads\/main'/);
  assert.match(validateJob, /permissions:\n      contents: write/);
  assert.match(validateJob, /ref: \$\{\{ github\.sha \}\}/);
  assert.match(
    release,
    /^      release_tag:\n(?:        .*\n)+?        required: false$/m,
  );
  assert.match(
    validateJob,
    /Expected exactly one draft GitHub Release when release_tag is omitted/,
  );
  assert.match(validateJob, /\.draft == true and \.prerelease == false/);
  assert.match(validateJob, /node scripts\/check-release-tag\.mjs "\$RELEASE_TAG"/);
  assert.match(
    validateJob,
    /repos\/\$GITHUB_REPOSITORY\/releases\?per_page=100/,
  );
  assert.match(validateJob, /select\(\.tag_name == \$tag\)/);
  assert.match(validateJob, /refs\/tags\/\$RELEASE_TAG/);
  assert.match(validateJob, /repos\/\$GITHUB_REPOSITORY\/git\/refs/);
  assert.match(validateJob, /git merge-base --is-ancestor "\$RELEASE_SHA" origin\/main/);
  assert.match(validateJob, /release-snapshot\.json/);
  assert.match(validateJob, /Mark the in-flight draft as a prerelease/);
  assert.match(
    validateJob,
    /(?:-f draft=true[\s\S]*-f prerelease=true|\{draft: true, prerelease: true\})/,
  );
  assert.match(validateJob, /Failed to keep release \$RELEASE_ID as a draft after marking it in-flight| -f draft=true/);
  assert.doesNotMatch(validateJob, /-f draft=false/);
  assert.match(release, /ref: \$\{\{ needs\.validate\.outputs\.sha \}\}/);
});

test("release documentation is built from the validated tag and promoted only after staged checks", () => {
  const build = workflowJob(workflows["release.yml"], "build_docs");
  const publish = workflowJob(workflows["release.yml"], "publish_docs");

  assert.match(build, /^    needs: validate$/m);
  assert.doesNotMatch(build, /^    environment:/m);
  assert.match(publish, /^    needs: \[validate, build_docs, finalize_release\]$/m);
  assert.match(publish, /^    environment:\n      name: docs-production$/m);
  assert.match(build, /^    permissions:\n      contents: read$/m);
  assert.match(publish, /^    permissions:\n      contents: read$/m);
  assert.doesNotMatch(`${build}\n${publish}`, /id-token: write|contents: write/);
  assert.match(build, /BASE_PATH: \/docs/);
  assert.match(build, /RELEASE_SHA: \$\{\{ needs\.validate\.outputs\.sha \}\}/);
  assert.doesNotMatch(build, /VERCEL_TOKEN|docs-production/);
  assert.match(publish, /VERCEL_TOKEN: \$\{\{ secrets\.VERCEL_TOKEN \}\}/);
  assert.equal(
    Object.hasOwn(docsPackage.dependencies ?? {}, "vercel"),
    false,
  );
  if (Object.hasOwn(docsPackage.devDependencies ?? {}, "vercel")) {
    assert.match(docsPackage.devDependencies.vercel, /^\d+\.\d+\.\d+$/);
  }
  assert.match(build, /pnpm --dir docs-site install --frozen-lockfile/);
  assert.match(build, /ref: \$\{\{ needs\.validate\.outputs\.sha \}\}/);
  assert.match(build, /persist-credentials: false/);
  assert.match(build, /test "\$\(git rev-parse HEAD\)" = "\$RELEASE_SHA"/);
  assert.match(build, /pnpm --dir docs-site build/);
  assert.match(build, /pnpm --dir docs-site test:vercel-output/);
  assert.match(build, /pnpm --dir docs-site package:vercel/);
  assert.match(build, /\.vercel\/output\/static\/docs\/index\.html/);
  assert.match(build, /cd \.vercel\/output/);
  assert.match(build, /find \. -type f -print0/);
  assert.match(build, /tidebreak-docs-\$RELEASE_TAG\.sha256/);
  assert.match(build, /name: tidebreak-docs-\$\{\{ needs\.validate\.outputs\.tag \}\}-prebuilt/);
  assert.doesNotMatch(publish, /docs-site install|docs-site build/);
  assert.equal(vercelCliPackage.dependencies.vercel, "59.0.0");
  assert.match(publish, /sparse-checkout: \.github\/vercel-cli/);
  assert.match(publish, /persist-credentials: false/);
  assert.match(
    publish,
    /pnpm --dir \.github\/vercel-cli install --frozen-lockfile --ignore-scripts/,
  );
  assert.doesNotMatch(publish, /pnpm (?:add --global|dlx)|npx/);
  assert.match(publish, /actions\/download-artifact@[0-9a-f]{40} # v8/);
  assert.match(publish, /name: tidebreak-docs-\$\{\{ needs\.validate\.outputs\.tag \}\}-manifest/);
  assert.match(publish, /sha256sum --check --strict/);
  assert.match(publish, /asset_path=.*\/docs\/_next\//);
  for (const directive of [
    "default-src 'self'",
    "object-src 'none'",
    "frame-ancestors 'none'",
  ]) {
    assert.equal(
      publish.split(`content-security-policy:.*${directive}`).length - 1,
      2,
      `staged and promoted docs must both verify ${directive}`,
    );
  }
  assert.match(publish, /x-frame-options: DENY/);
  assert.match(publish, /\.id == \$id and \.readyState == "READY"/);
  assert.match(publish, /--prebuilt/);
  assert.match(publish, /--prod/);
  assert.match(publish, /--skip-domain/);
  assert.match(publish, /--meta "releaseTag=\$RELEASE_TAG"/);
  assert.match(publish, /--meta "releaseSha=\$RELEASE_SHA"/);
  assert.match(publish, /jq -ce '\.deployment \/\/ \.'/);
  assert.match(publish, /deployment_url=.*jq -er \.url/);
  assert.match(publish, /deployment_id=.*jq -er \.id/);
  assert.doesNotMatch(publish, /(?:^|\s)--scope(?:\s|$)/m);
  assert.doesNotMatch(publish, /VERCEL_SCOPE/);
  assert.match(publish, /url\.searchParams\.set\("teamId", teamId\)/);
  assert.match(
    publish,
    /\/v10\/projects\/" \+ projectId \+ "\/promote\/" \+ deploymentId/,
  );
  assert.match(publish, /\/v13\/deployments\/tidebreak-docs\.vercel\.app/);
  assert.match(
    publish,
    /JSON\.stringify\(\{\s*id: deployment\.id,\s*readyState: deployment\.readyState,\s*\}\)/,
  );
  assert.match(publish, /Vercel promote failed: " \+ response\.status/);
  assert.doesNotMatch(publish, /\+ await response\.text\(\)/);
  assert.doesNotMatch(publish, /--token/);
  assert.match(publish, /\/docs\/quickstart\//);
  assert.match(publish, /\/docs\/search-index\.json/);
  assert.match(publish, /\/docs\/sitemap\.xml/);
  assert.match(publish, /\.github\/vercel-cli\/node_modules\/\.bin\/vercel curl/);
  assert.match(publish, /\.github\/vercel-cli\/node_modules\/\.bin\/vercel promote/);
  assert.doesNotMatch(publish, /vercel inspect/);

  const deploy = publish.indexOf("Create an unaliased production deployment");
  const verify = publish.indexOf("Verify the transferred documentation");
  const smoke = publish.indexOf("Smoke-test the staged deployment");
  const promote = publish.indexOf("Promote the verified deployment");
  const production = publish.indexOf("Verify the production alias");
  assert.ok(
    verify !== -1 &&
      verify < deploy &&
      deploy < smoke &&
      smoke < promote &&
      promote < production,
    "docs must be verified, staged, checked, promoted, and then verified in that order",
  );

  const packageOutput = build.indexOf("pnpm --dir docs-site package:vercel");
  const digestOutput = build.indexOf("find . -type f -print0");
  assert.ok(
    packageOutput !== -1 && packageOutput < digestOutput,
    "the digest must cover the packaged Vercel output, including config.json",
  );
});

test("cache warming cannot access production credentials or publish", () => {
  const cache = workflows["cache-macos.yml"];
  assert.ok(cache);
  assert.match(cache, /^on:\n  push:\n    branches: \[main\]/m);
  assert.match(cache, /^  workflow_dispatch:$/m);
  assert.doesNotMatch(cache, /^\s*pull_request(?:_target)?:/m);
  assert.match(cache, /^  cargo-downloads:$/m);
  assert.match(cache, /^    needs: cargo-downloads$/m);
  assert.match(cache, /cargo fetch --locked --target aarch64-apple-darwin/);
  assert.match(cache, /cancel-in-progress: false/);
  assert.match(cache, /--no-bundle --ci/);
  assert.match(cache, /continue-on-error: true/);
  assert.doesNotMatch(cache, /^    environment:/m);
  assert.doesNotMatch(cache, /secrets\./);
  assert.doesNotMatch(cache, /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_/);
  assert.doesNotMatch(cache, /actions\/upload-artifact/);

  const release = workflows["release.yml"];
  assert.doesNotMatch(release, /cache_warm_only/);
  assert.doesNotMatch(release, /^  warm-macos-cache:/m);

  for (const workflow of [cache, release]) {
    const downloadCaches = [
      ...workflow.matchAll(
        /- name: Cache Cargo downloads[\s\S]*?(?=\n\s+- (?:name:|uses:))/g,
      ),
    ].map((match) => match[0]);
    assert.ok(downloadCaches.length > 0);
    for (const step of downloadCaches) {
      assert.match(
        step,
        /shared-key: (?:macos-release-cargo-registry-v2|windows-release-cargo-registry-v1|linux-release-cargo-registry-v1)-\$\{\{ hashFiles\('Cargo\.lock'\) \}\}/,
      );
      assert.match(step, /add-rust-environment-hash-key: "false"/);
      assert.match(step, /cache-targets: false/);
    }
  }
});


// ---------------------------------------------------------------------------
// Release-pipeline invariant tests.
//
// These tests assert security invariants (credential isolation, ordering,
// cache hygiene) rather than exact step names or shell snippets, so a
// workflow refactor that renames a step or rewords a script does not break
// them — as long as the invariant holds.
// ---------------------------------------------------------------------------

// Helper: every `- name:` label in a job, in order.
function stepNames(job) {
  return [...job.matchAll(/^\s+- name: (.+)$/gm)].map((m) => m[1]);
}

test("credential-free compile jobs never load production secrets", () => {
  const release = workflows["release.yml"];
  for (const jobName of ["prepare_macos", "prepare_windows"]) {
    const job = workflowJob(release, jobName);
    assert.doesNotMatch(job, /^    environment:/m);
    assert.doesNotMatch(job, /secrets\./);
    assert.doesNotMatch(job, /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_/);
  }
  // The credential-free compile must save its cache before reporting a
  // failed compile, so partial work is reusable.
  for (const jobName of ["prepare_macos", "prepare_windows"]) {
    const job = workflowJob(release, jobName);
    const names = stepNames(job);
    const saveIdx = names.findIndex((n) => /Save.*cache/i.test(n));
    const failIdx = names.findIndex((n) => /Require.*successful.*compilation/i.test(n));
    assert.ok(saveIdx !== -1, `${jobName} must have a cache-save step`);
    assert.ok(failIdx !== -1, `${jobName} must have a compile-failure step`);
    assert.ok(saveIdx < failIdx, `${jobName} must save cache before reporting failure`);
  }
});

test("credentialed packaging jobs restore caches before loading secrets", () => {
  const release = workflows["release.yml"];
  for (const { name, validate } of [
    { name: "build_macos", validate: "Validate production signing configuration" },
    { name: "build_windows", validate: "Validate updater signing configuration" },
  ]) {
    const job = workflowJob(release, name);
    assertCachesRestoreBeforeSigningMaterial(job, name, validate);
    // Credentialed jobs must never save a cache.
    assert.doesNotMatch(job, /actions\/cache\/save/);
  }
});

test("credentialed packaging policy rejects cache restores after signing material", () => {
  const release = workflows["release.yml"];
  const name = "build_macos";
  const validate = "Validate production signing configuration";
  const job = `${workflowJob(release, name)}\n    - name: Restore unsigned Rust build cache\n      run: echo unsafe\n`;
  assert.throws(
    () => assertCachesRestoreBeforeSigningMaterial(job, name, validate),
    /must restore Restore unsigned Rust build cache before loading secrets/,
  );
});

test("the updater private key is isolated from compilation", () => {
  const release = workflows["release.yml"];
  for (const jobName of ["prepare_macos", "prepare_windows"]) {
    assert.doesNotMatch(
      workflowJob(release, jobName),
      /TAURI_SIGNING_PRIVATE_KEY/,
    );
  }
  // The bundle/sign step in the macOS production job must not reference it.
  const buildMac = workflowJob(release, "build_macos");
  const names = stepNames(buildMac);
  const bundleIdx = names.findIndex((n) => /bundle|sign/i.test(n) && !/notar/i.test(n));
  const signIdx = names.findIndex((n) => /signing configuration/i.test(n));
  if (bundleIdx !== -1 && signIdx !== -1) {
    const step = buildMac.match(
      new RegExp(`- name: ${names[bundleIdx]}[\\s\\S]*?(?=\\n\\s+- name:)`),
    )?.[0];
    if (step) {
      assert.doesNotMatch(step, /TAURI_SIGNING_PRIVATE_KEY/);
    }
  }
  assert.match(release, /tauri signer sign "\$updater_path"/);
  assert.doesNotMatch(release, /cargo tauri signer sign/);
  assert.doesNotMatch(release, /createUpdaterArtifacts/);
});

test("signing jobs run installers before loading signing material", () => {
  for (const { file, name, job, validate } of desktopSigningJobs()) {
    const label = `${name} (${file})`;
    const secretsAt = firstSigningMaterialIndex(job, validate);
    assert.notEqual(secretsAt, -1, `${label} must still load signing material`);
    const pnpmAt = job.search(/pnpm\/action-setup@[0-9a-f]{40}/);
    const nodeAt = job.search(/actions\/setup-node@[0-9a-f]{40}/);
    assert.ok(pnpmAt !== -1, `${label} must set up pnpm`);
    assert.ok(nodeAt !== -1, `${label} must set up Node`);
    assert.ok(pnpmAt < secretsAt, `${label} must set up pnpm before signing material`);
    assert.ok(nodeAt < secretsAt, `${label} must set up Node before signing material`);
    // The actual install command (not just the setup) must also come before
    // signing material — a moved install step is a supply-chain risk.
    const installAt = job.search(
      /pnpm(?: --dir \.github\/tauri-cli)? install --frozen-lockfile --ignore-scripts/,
    );
    assert.ok(installAt !== -1, `${label} must have a frozen-lockfile install step`);
    assert.ok(installAt < secretsAt, `${label} must install dependencies before signing material`);
    // pnpm must be pinned to an exact version, not floating.
    assert.match(job, /version: 10\.18\.3\n/, `${label} must pin pnpm 10.18.3`);
    // Installs must be frozen-lockfile with lifecycle scripts disabled.
    assert.match(
      job,
      /pnpm(?: --dir \.github\/tauri-cli)? install --frozen-lockfile --ignore-scripts/,
      `${label} must install with --frozen-lockfile --ignore-scripts`,
    );
    if (/Install pinned Tauri bundler/.test(job)) {
      assert.doesNotMatch(job, /mozilla-actions\/sccache-action/);
      assert.doesNotMatch(job, /Swatinem\/rust-cache/);
    } else {
      const rustCache = cargoDownloadCache(job);
      assert.ok(rustCache, `${label} missing Cargo download cache`);
      assert.match(rustCache, /save-if: false/);
    }
  }
});

test("prepared binaries are checksum-verified before signing material loads", () => {
  const release = workflows["release.yml"];
  for (const { name, validate } of [
    { name: "build_macos", validate: "Validate production signing configuration" },
    { name: "build_windows", validate: "Validate updater signing configuration" },
  ]) {
    const job = workflowJob(release, name);
    if (!/Download prepared/.test(job)) return;
    const secretsAt = firstSigningMaterialIndex(job, validate);
    const verifyIdx = job.search(/sha256.*--check|shasum.*--check/);
    assert.ok(verifyIdx !== -1, `${name} must verify prepared input checksums`);
    assert.ok(verifyIdx < secretsAt, `${name} must verify checksums before loading secrets`);
  }
});

test("restored product binaries are discarded before the packaging build", () => {
  const release = workflows["release.yml"];
  for (const name of ["build_macos", "prepare_windows", "build_windows", "build_linux"]) {
    const job = workflowJob(release, name);
    const names = stepNames(job);
    const restoreIdx = names.findIndex((n) => /Restore.*cache/i.test(n));
    const discardIdx = names.findIndex((n) => /Discard.*product/i.test(n));
    const buildIdx = names.findIndex((n) =>
      /Build.*Tauri|Bundle.*Tauri|Compile.*production credentials/i.test(n),
    );
    if (restoreIdx === -1) {
      assert.equal(discardIdx, -1, `${name}: discard without a cache restore is stale`);
      continue;
    }
    assert.notEqual(discardIdx, -1, `${name}: restored products must be discarded`);
    assert.ok(restoreIdx < discardIdx, `${name}: cache restore must come before discard`);
    assert.ok(discardIdx < buildIdx, `${name}: discard must come before the build`);
    // The discard step must remove every final product binary the warm cache
    // can restore. The Linux CLI patterns are exact because `\b` also matches
    // before the hyphen in `tidebreak-desktop` and `tidebreak-host-broker`.
    const discardStep = job.match(
      new RegExp(`- name: ${names[discardIdx].replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}[\\s\\S]*?(?=\\n\\s+- name:)`),
    )?.[0];
    assert.ok(discardStep, `${name}: discard step content not found`);
    const products = [
      /release\/tidebreak-desktop/,
      /release\/tidebreak-host-broker/,
      name === "build_linux"
        ? /release\/tidebreak(?:\s|$)/m
        : /release\/tidebreak\b/,
      /binaries\/tidebreak-host-broker/,
      name === "build_linux"
        ? /binaries\/tidebreak-\$\{\{ matrix\.target \}\}(?:\s|$)/m
        : /binaries\/tidebreak\b/,
    ];
    for (const product of products) {
      assert.match(discardStep, product, `${name}: discard must remove ${product}`);
    }
  }
});

test("release version changes invalidate restored Rust artifacts", () => {
  for (const crateName of ["tidebreak-cli", "tidebreak-core", "tidebreak-server"]) {
    const buildScript = readFileSync(
      repositoryFile("crates", crateName, "build.rs"),
      "utf8",
    );
    assert.match(buildScript, /cargo::rerun-if-env-changed=TIDEBREAK_VERSION/);
  }
});

test("cache archives never include bundles, signatures, or keychains", () => {
  const release = workflows["release.yml"];
  const cache = workflows["cache-macos.yml"];
  // Only match `path:` blocks inside cache steps. The path list must not
  // cross a step boundary (a line starting with `      - name:` or
  // `      - uses:`), so filter out matches that span multiple steps.
  for (const source of [release, cache]) {
    const cacheSteps = [...source.matchAll(
      /path: \|\n([\s\S]*?)\n\s+key: [$a-zA-Z]/g,
    )]
      .map((m) => m[1])
      .filter((paths) => !/^\s+- (?:name|uses):/m.test(paths));
    for (const paths of cacheSteps) {
      assert.doesNotMatch(paths, /pdfium/i);
      assert.doesNotMatch(paths, /bundle|\.app\b|dmg|signature|keychain/i);
    }
  }
});

test("macOS notarization happens after bundling and before artifact verification", () => {
  const release = workflows["release.yml"];
  const names = stepNames(release);
  const bundleIdx = names.findIndex((n) => /bundle.*sign/i.test(n));
  const notarizeIdx = names.findIndex((n) => /notar/i.test(n) && /dmg|staple/i.test(n));
  const verifyIdx = names.findIndex((n) => /verify.*collect.*artifact/i.test(n));
  assert.ok(bundleIdx !== -1, "release must have a bundle/sign step");
  assert.ok(notarizeIdx !== -1, "release must have a notarization step");
  assert.ok(verifyIdx !== -1, "release must have an artifact verification step");
  assert.ok(bundleIdx < notarizeIdx, "bundling must come before notarization");
  assert.ok(notarizeIdx < verifyIdx, "notarization must come before artifact verification");
  // The notarization step must submit to notarytool and staple both DMG and app.
  // Find the step that actually contains `notarytool submit` — it may be
  // named differently across workflow revisions.
  const notarytoolIdx = release.indexOf("notarytool submit");
  assert.ok(notarytoolIdx !== -1, "release must submit to notarytool");
  const stepStart = release.lastIndexOf("- name:", notarytoolIdx);
  const stepEnd = release.indexOf("\n      - name:", notarytoolIdx);
  const notarizeStep = release.slice(stepStart, stepEnd !== -1 ? stepEnd : undefined);
  assert.match(notarizeStep, /notarytool submit/);
  assert.match(notarizeStep, /stapler staple/);
  assert.match(notarizeStep, /stapler validate/);
  // The App Store Connect key ordering (after bundling) is asserted only
  // when the workflow uses the single-submission pattern where the key step
  // is separate from the bundle and the notary submission is a standalone
  // step. The legacy pattern (Tauri notarizes during `tauri bundle`) loads
  // the key before bundling, which is correct for that pattern.
  const keyIdx = names.findIndex((n) => /Prepare App Store Connect key/i.test(n));
  if (keyIdx !== -1 && bundleIdx !== -1 && notarizeIdx !== -1) {
    const singleSubmission = /dmg.*app|app.*dmg/i.test(names[notarizeIdx]);
    if (singleSubmission) {
      assert.ok(bundleIdx < keyIdx, "the notary key must load after bundling in the single-submission pattern");
      assert.ok(keyIdx <= notarizeIdx, "the notary key must load before notarization");
    }
  }
  // Same invariant for staging.
  const staging = workflows["staging-publish.yml"];
  if (staging) {
    const stagingNames = stepNames(staging);
    const sBundle = stagingNames.findIndex((n) => /bundle.*sign|build.*sign/i.test(n));
    const sKey = stagingNames.findIndex((n) => /Prepare App Store Connect key/i.test(n));
    const sNotarize = stagingNames.findIndex((n) => /notar/i.test(n) && /dmg|staple/i.test(n));
    if (sBundle !== -1 && sKey !== -1 && sNotarize !== -1) {
      const sSingle = /dmg.*app|app.*dmg/i.test(stagingNames[sNotarize]);
      if (sSingle) {
        assert.ok(sBundle < sKey, "staging: the notary key must load after bundling");
        assert.ok(sKey <= sNotarize, "staging: the notary key must load before notarization");
      }
    }
  }
});

test("Linux packaging writes no shared cache before loading updater material", () => {
  const release = workflows["release.yml"];
  assert.doesNotMatch(release, /^  prepare_linux:/m);
  const buildJob = workflowJob(release, "build_linux");
  assert.match(buildJob, /needs: \[validate, inspect_hosted\]/);
  assert.match(buildJob, /ubuntu-22\.04/);
  assert.match(buildJob, /runs-on: \$\{\{ matrix\.runner \}\}/);
  assert.match(buildJob, /target: x86_64-unknown-linux-gnu/);
  assert.match(buildJob, /runner: ubuntu-22\.04/);
  assert.match(buildJob, /target: aarch64-unknown-linux-gnu/);
  assert.match(buildJob, /runner: ubuntu-22\.04-arm/);
  assert.match(buildJob, /libwebkit2gtk-4\.1-dev/);
  assert.match(buildJob, /xdg-utils/);
  assert.match(buildJob, /scripts\/install-linux-apt-packages\.sh/);
  // The credentialed packaging job must never save a cache.
  assert.doesNotMatch(buildJob, /actions\/cache\/save/);
  // Cargo download cache must be restore-only.
  const rustCache = cargoDownloadCache(buildJob);
  assert.ok(rustCache);
  assert.match(rustCache, /save-if: false/);
  // Packages must be built before updater signing material is loaded.
  const names = stepNames(buildJob);
  const buildIdx = names.findIndex((n) => /Build.*Tauri.*Linux/i.test(n));
  const signIdx = names.findIndex((n) => /Verify.*collect.*Linux.*artifact/i.test(n));
  if (buildIdx !== -1 && signIdx !== -1) {
    assert.ok(buildIdx < signIdx, "Linux packages must be built before updater signing");
  }
  assert.match(buildJob, /--bundles appimage,deb/);
  assert.match(buildJob, /\.AppImage/);
  assert.match(buildJob, /\.deb/);
  assert.match(buildJob, /tauri signer sign/);
});

test("an existing immutable release resumes without rebuilding or overwriting", () => {
  const release = workflows["release.yml"];
  const inspectJob = workflowJob(release, "inspect_hosted");
  const prepareJob = workflowJob(release, "prepare_macos");
  const signedBuildJob = workflowJob(release, "build_macos");
  const publishJob = workflowJob(release, "publish");

  assert.match(inspectJob, /id-token: write/);
  assert.match(inspectJob, /ref: \$\{\{ github\.sha \}\}/);
  assert.match(inspectJob, /prepare-published-release\.mjs/);
  assert.match(inspectJob, /Validated the complete immutable release/);
  assert.match(prepareJob, /needs\.inspect_hosted\.outputs\.exists != 'true'/);
  assert.match(
    signedBuildJob,
    /needs\.inspect_hosted\.outputs\.exists != 'true'/,
  );
  assert.match(
    workflowJob(release, "prepare_windows"),
    /needs\.inspect_hosted\.outputs\.exists != 'true'/,
  );
  assert.match(
    workflowJob(release, "build_windows"),
    /needs\.inspect_hosted\.outputs\.exists != 'true'/,
  );
  assert.match(
    publishJob,
    /needs\.inspect_hosted\.outputs\.exists == 'true'/,
  );
  assert.match(publishJob, /Resume from the hosted immutable release/);
  assert.match(publishJob, /ref: \$\{\{ github\.sha \}\}/);

  const immutableUpload = publishJob.match(
    /- name: Upload immutable release files[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(immutableUpload);
  assert.match(
    immutableUpload,
    /if: \$\{\{ needs\.inspect_hosted\.outputs\.exists != 'true' \}\}/,
  );
});

test("source SBOM generation is isolated from production credentials", () => {
  const release = workflows["release.yml"];
  const sbomJob = workflowJob(release, "source_sbom");
  const publishJob = workflowJob(workflows["release.yml"], "publish");

  assert.match(sbomJob, /needs: \[validate, inspect_hosted\]/);
  assert.match(sbomJob, /permissions:\n      contents: read/);
  assert.doesNotMatch(sbomJob, /id-token:/);
  assert.doesNotMatch(sbomJob, /attestations:/);
  assert.doesNotMatch(sbomJob, /artifact-metadata:/);
  assert.doesNotMatch(sbomJob, /\n    environment:/);
  assert.doesNotMatch(sbomJob, /AWS_|DOWNLOADS_|RELEASE_BASE_URL|vars\.|secrets\./);
  assert.match(sbomJob, /ref: \$\{\{ needs\.validate\.outputs\.sha \}\}/);
  assert.match(sbomJob, /run: mkdir -p source-sbom/);
  assert.match(
    sbomJob,
    /uses: anchore\/sbom-action@[0-9a-f]{40} # v0\.24\.0/,
  );
  assert.match(sbomJob, /syft-version: v1\.51\.0/);
  assert.match(sbomJob, /format: spdx-json/);
  assert.match(sbomJob, /upload-artifact: false/);
  assert.match(sbomJob, /upload-release-assets: false/);
  assert.match(
    sbomJob,
    /uses: actions\/upload-artifact@[0-9a-f]{40} # v7/,
  );
  assert.match(sbomJob, /name: tidebreak-source-sbom-\$\{\{ needs\.validate\.outputs\.version \}\}/);
  assert.doesNotMatch(publishJob, /anchore\/sbom-action/);

  assert.match(publishJob, /needs: \[validate, inspect_hosted, build_macos, build_windows, build_linux, source_sbom, finalize_release\]/);
  assert.match(publishJob, /needs\.source_sbom\.result == 'success'/);
  assert.match(
    publishJob,
    /uses: actions\/download-artifact@[0-9a-f]{40} # v8/,
  );
  assert.match(publishJob, /name: tidebreak-source-sbom-\$\{\{ needs\.validate\.outputs\.version \}\}/);
  assert.match(publishJob, /sha256sum --check --strict/);
});

test("public releases attest provenance without treating the source SBOM as an installer SBOM", () => {
  const publishJob = workflowJob(workflows["release.yml"], "publish");

  assert.match(publishJob, /attestations: write/);
  assert.match(publishJob, /id-token: write/);
  assert.match(publishJob, /github\.event\.repository\.visibility == 'public'/);

  const attestations = publishJob.match(
    /uses: actions\/attest@[0-9a-f]{40} # v4\.2\.2/g,
  );
  assert.equal(attestations?.length, 1);
  assert.match(publishJob, /subject-checksums: \$\{\{ runner\.temp \}\}\/immutable-release-files\.sha256/);
  assert.doesNotMatch(publishJob, /sbom-path:/);
  assert.doesNotMatch(publishJob, /release-artifacts\.sha256/);

  const provenanceIndex = publishJob.indexOf("- name: Attest immutable release provenance");
  const awsIndex = publishJob.indexOf("- name: Configure AWS credentials");
  assert.ok(provenanceIndex !== -1 && awsIndex !== -1);
  assert.ok(provenanceIndex < awsIndex);
});

test("GitHub release assets are attached before immutable publication", () => {
  const release = workflows["release.yml"];
  const attachJob = workflowJob(release, "attach_downloads");
  const finalizeJob = workflowJob(release, "finalize_release");
  const publishJob = workflowJob(release, "publish");

  assert.match(attachJob, /needs: \[validate, inspect_hosted, build_macos, build_windows, build_linux, source_sbom\]/);
  assert.match(attachJob, /contents: write/);
  assert.doesNotMatch(attachJob, /^    environment:/m);
  assert.doesNotMatch(attachJob, /secrets\./);
  assert.doesNotMatch(attachJob, /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_S3/);

  assert.match(attachJob, /name: tidebreak-macos-universal-/);
  assert.match(attachJob, /name: tidebreak-windows-x86_64-/);
  assert.match(attachJob, /name: tidebreak-windows-aarch64-/);
  assert.match(attachJob, /name: tidebreak-linux-x86_64-/);
  assert.match(attachJob, /name: tidebreak-linux-aarch64-/);
  assert.match(attachJob, /name: tidebreak-source-sbom-/);
  assert.match(attachJob, /gh release download "\$RELEASE_TAG"/);
  assert.match(attachJob, /sha256sum --check --strict/);
  assert.match(attachJob, /Tidebreak-macos-universal\.dmg/);
  assert.match(attachJob, /Tidebreak-macos-apple-silicon\.dmg/);
  assert.match(attachJob, /Tidebreak-windows-x86_64-setup\.exe/);
  assert.match(attachJob, /Tidebreak-windows-aarch64-setup\.exe/);
  assert.match(attachJob, /Tidebreak-linux-x86_64\.AppImage/);
  assert.match(attachJob, /Tidebreak-linux-x86_64\.deb/);
  assert.match(attachJob, /Tidebreak-linux-aarch64\.AppImage/);
  assert.match(attachJob, /Tidebreak-linux-aarch64\.deb/);
  assert.match(attachJob, /\.app\.zip/);
  assert.match(attachJob, /\.app\.tar\.gz/);
  assert.match(attachJob, /\.app\.tar\.gz\.sig/);
  assert.match(
    attachJob,
    /Tidebreak_\$\{TIDEBREAK_VERSION\}_(?:x86_64|\$\{arch\})\.deb\.sig/,
  );
  if (/name: tidebreak-windows-aarch64-/.test(attachJob)) {
    assert.match(attachJob, /name: tidebreak-linux-aarch64-/);
    assert.match(attachJob, /Tidebreak-windows-aarch64-setup\.exe/);
    assert.match(attachJob, /Tidebreak-linux-aarch64\.AppImage/);
    assert.match(attachJob, /Tidebreak-linux-aarch64\.deb/);
    assert.match(
      attachJob,
      /Tidebreak_\$\{TIDEBREAK_VERSION\}_(?:aarch64|\$\{arch\})\.deb\.sig/,
    );
  }
  assert.match(
    attachJob,
    /Tidebreak_\$\{TIDEBREAK_VERSION\}_x86_64\.deb\.sig/,
  );
  assert.match(
    attachJob,
    /Tidebreak_\$\{TIDEBREAK_VERSION\}_aarch64\.deb\.sig/,
  );
  assert.match(attachJob, /gh release upload "\$RELEASE_TAG"/);
  assert.match(attachJob, /if \[\[ "\$RELEASE_DRAFT" = true \]\]/);
  assert.match(attachJob, /releases\/\$RELEASE_ID\/assets/);
  assert.match(attachJob, /expected-release-assets/);
  assert.match(attachJob, /actual-release-assets/);
  assert.match(attachJob, /diff -u/);

  assert.match(finalizeJob, /needs: \[validate, attach_downloads\]/);
  assert.match(finalizeJob, /contents: write/);
  assert.match(finalizeJob, /commits\/\$RELEASE_TAG/);
  assert.match(finalizeJob, /Release tag \$RELEASE_TAG moved after validation/);
  assert.match(finalizeJob, /draft: false/);
  assert.match(finalizeJob, /make_latest: "true"/);
  assert.match(finalizeJob, /published_at=\$published_at/);
  assert.match(publishJob, /needs\.finalize_release\.result == 'success'/);
  assert.match(
    publishJob,
    /RELEASE_PUBLISHED_AT: \$\{\{ needs\.finalize_release\.outputs\.published_at \}\}/,
  );
  assert.match(publishJob, /Recover build inputs from the immutable GitHub Release/);
  assert.match(publishJob, /recovery\/Tidebreak-macos-universal\.dmg/);

  const macDownloadLink = readFileSync(repositoryFile("README.md"), "utf8")
    .match(
      /releases\/latest\/download\/(Tidebreak-macos-[\w-]+\.dmg)/,
    )?.[1];
  assert.ok(macDownloadLink, "the README must publish a macOS download link");
  assert.ok(
    attachJob.includes(`downloads/${macDownloadLink}`),
    `attach_downloads must upload ${macDownloadLink}`,
  );

  for (const [platform, pattern] of [
    ["Windows", /releases\/latest\/download\/(Tidebreak-windows-[\w-]+\.exe)/],
    ["Linux AppImage", /releases\/latest\/download\/(Tidebreak-linux-[\w-]+\.AppImage)/],
    ["Linux Debian", /releases\/latest\/download\/(Tidebreak-linux-[\w-]+\.deb)/],
  ]) {
    const downloadLink = readFileSync(repositoryFile("README.md"), "utf8")
      .match(pattern)?.[1];
    assert.ok(downloadLink, `the README must publish a ${platform} download link`);
    assert.ok(
      attachJob.includes(`downloads/${downloadLink}`),
      `attach_downloads must upload ${downloadLink}`,
    );
  }
});

test("universal macOS release and staging packages contain both slices", () => {
  const releasePrepare = workflowJob(workflows["release.yml"], "prepare_macos");
  const releaseBuild = workflowJob(workflows["release.yml"], "build_macos");
  if (!/--target universal-apple-darwin/.test(releaseBuild)) {
    return;
  }

  const stagingBuild = workflowJob(
    workflows["staging-publish.yml"],
    "build_macos_staging",
  );
  const warm = workflows["cache-macos.yml"];
  const sidecarPreparation = readFileSync(
    repositoryFile("crates/tidebreak-desktop/scripts/prepare-sidecar.mjs"),
    "utf8",
  );

  assert.match(sidecarPreparation, /triple === "universal-apple-darwin"/);
  assert.match(
    sidecarPreparation,
    /\["aarch64-apple-darwin", "x86_64-apple-darwin"\]/,
  );
  assert.match(
    sidecarPreparation,
    /"lipo",\s*\["-create", \.\.\.stagedSidecars, "-output", destination\]/,
  );

  const preparedArtifacts = /Archive prepared macOS inputs/.test(releasePrepare);
  const releaseCompile = preparedArtifacts ? releasePrepare : releaseBuild;
  for (const job of [releaseCompile, stagingBuild, warm]) {
    assert.match(job, /rustup target add aarch64-apple-darwin x86_64-apple-darwin/);
    assert.match(job, /--target universal-apple-darwin/);
  }
  if (preparedArtifacts) {
    assert.match(releaseBuild, /--target universal-apple-darwin/);
    assert.doesNotMatch(releaseBuild, /rustup target add/);
  }

  for (const job of [releaseBuild, stagingBuild]) {
    assert.match(job, /timeout-minutes: 90/);
    assert.match(job, /lipo -archs "\$app_path\/Contents\/MacOS\/\$executable"/);
    assert.match(job, /\$binary_arches" = \*arm64\*/);
    assert.match(job, /\$binary_arches" = \*x86_64\*/);
    assert.match(job, /sidecar="\$app_path\/Contents\/MacOS\/tidebreak-host-broker"/);
    assert.match(job, /sidecar_arches="\$\(lipo -archs "\$sidecar"\)"/);
    assert.match(job, /cli_sidecar="\$app_path\/Contents\/MacOS\/tidebreak"/);
    assert.match(job, /cli_arches="\$\(lipo -archs "\$cli_sidecar"\)"/);
  }
});

test("staging smoke-tests packaged GitHub CLI discovery before upload", () => {
  const stagingBuild = workflowJob(
    workflows["staging-publish.yml"],
    "build_macos_staging",
  );
  const verifyAt = stagingBuild.indexOf("Verify and collect signed artifacts");
  const smokeAt = stagingBuild.indexOf(
    "Smoke-test packaged GitHub CLI discovery",
  );
  const uploadAt = stagingBuild.indexOf("Upload verified macOS artifacts");
  assert.ok(
    verifyAt < smokeAt && smokeAt < uploadAt,
    "the packaged-app smoke check must run after verification and before upload",
  );
  assert.match(
    stagingBuild.slice(smokeAt, uploadAt),
    /scripts\/smoke-packaged-gh-discovery\.sh "\$\{app_paths\[0\]\}"/,
  );

  assert.match(packagedGhDiscoverySmoke, /finder_path="\/usr\/bin:\/bin:\/usr\/sbin:\/sbin"/);
  assert.match(packagedGhDiscoverySmoke, /\/usr\/bin\/env -i/);
  assert.match(packagedGhDiscoverySmoke, /CFFIXED_USER_HOME="\$profile_home"/);
  assert.match(packagedGhDiscoverySmoke, /ZDOTDIR="\$shell_config"/);
  assert.match(
    packagedGhDiscoverySmoke,
    /export PATH="\$GH_SMOKE_LOGIN_BIN:\$PATH"/,
  );
  assert.match(packagedGhDiscoverySmoke, /listen_path="\$profile_data\/listen\.json"/);
  assert.match(packagedGhDiscoverySmoke, /\/code\/delivery\/repositories/);
  assert.match(packagedGhDiscoverySmoke, /\.capability\.found == true/);
  assert.match(packagedGhDiscoverySmoke, /auth status --json hosts/);
  assert.match(packagedGhDiscoverySmoke, /trap cleanup EXIT/);
  assert.match(packagedGhDiscoverySmoke, /rm -rf -- "\$smoke_root"/);
});

test("the packaged updater trusts the production signing key and endpoint", () => {
  const updater = tauriConfig.plugins?.updater;
  assert.ok(updater, "plugins.updater must exist when updater artifacts are built");
  assert.match(
    Buffer.from(updater.pubkey, "base64").toString("utf8"),
    /minisign public key/,
  );
  assert.deepEqual(updater.endpoints, [
    "https://downloads.brightwave.io/tidebreak/latest.json",
  ]);
});

test("staging desktop publishes only under the staging prefix", () => {
  const staging = workflows["staging.yml"];
  assert.ok(staging);
  // Staging polls main's tip. A per-push trigger only queued merges behind
  // each other on the one publish group, and GitHub cancelled the runs it
  // could not keep pending. It must never build a pull request's code.
  assert.match(
    staging,
    /^on:\n(?:  #[^\n]*\n)*  schedule:\n    - cron: "[^"]+"$/m,
  );
  assert.doesNotMatch(staging, /^\s*push:/m);
  // The poll builds only when a staged path moved since the build the channel
  // already hosts, which is the filter the push trigger's `paths` list was.
  assert.match(staging, /staging\/manifest\.json\?poll=/);
  assert.match(staging, /git diff --name-only "\$hosted" "\$STAGING_SHA"/);
  assert.match(
    staging,
    /if: \$\{\{ needs\.resolve\.outputs\.changed == 'true' \}\}/,
  );
  assert.match(staging, /^  workflow_dispatch:$/m);
  assert.doesNotMatch(staging, /^\s*pull_request(?:_target)?:/m);
  assert.match(staging, /^permissions:\n  contents: read$/m);
  assert.match(staging, /group: tidebreak-desktop-staging/);
  assert.match(staging, /cancel-in-progress: true/);
  assert.match(
    staging,
    /uses: \.\/\.github\/workflows\/(release|staging-publish)\.yml/,
  );
  assert.match(staging, /channel: staging/);
  assert.match(staging, /secrets: inherit/);
  assert.doesNotMatch(staging, /secrets\./);
  assert.doesNotMatch(staging, /desktop-production|tidebreak\/latest\.json/);

  const release = workflows["release.yml"];
  const stagingPublish = workflows["staging-publish.yml"];
  if (stagingPublish) {
    assert.match(stagingPublish, /^on:\n  workflow_call:\n/m);
    assert.doesNotMatch(stagingPublish, /github\.event_name == 'workflow_call'/);
    assert.doesNotMatch(stagingPublish, /^\s*pull_request(?:_target)?:/m);
    assert.match(stagingPublish, /^permissions:\n  contents: read$/m);
    assert.match(stagingPublish, /group: tidebreak-desktop-staging-build/);
    assert.match(stagingPublish, /cancel-in-progress: false/);
    assert.doesNotMatch(stagingPublish, /tidebreak\/latest\.json/);
    assert.doesNotMatch(release, /^  workflow_call:\n/m);
  } else {
    assert.match(release, /^  workflow_call:\n/m);
    assert.match(
      release,
      /inputs\.channel == 'staging'\n      && 'tidebreak-desktop-staging-build'/,
    );
    assert.match(
      release,
      /cancel-in-progress: \$\{\{ inputs\.channel == 'staging' \}\}/,
    );
  }

  const publishStaging = workflowJob(
    stagingPublish ?? release,
    "publish_staging",
  );
  assert.match(publishStaging, /environment:\n      name: desktop-staging/);
  assert.match(
    publishStaging,
    /RELEASE_BASE_URL: https:\/\/downloads\.brightwave\.io\/tidebreak\/staging/,
  );
  assert.match(publishStaging, /--channel staging/);
  assert.match(publishStaging, /tidebreak\/staging\/latest\.json/);
  assert.match(publishStaging, /desktop-channel\.mjs --assert-key staging/);
  assert.doesNotMatch(
    publishStaging,
    /s3:\/\/\$DOWNLOADS_S3_BUCKET\/tidebreak\/latest\.json/,
  );
  assert.doesNotMatch(publishStaging, /tidebreak\/releases\/v\$TIDEBREAK_VERSION/);

  const stagingOverlay = JSON.parse(
    readFileSync(
      repositoryFile("crates", "tidebreak-desktop", "tauri.staging.conf.json"),
      "utf8",
    ),
  );
  assert.equal(stagingOverlay.identifier, "io.brightwave.tidebreak.staging");
  const stagingPubkey = stagingOverlay.plugins.updater.pubkey;
  const productionPubkey = tauriConfig.plugins.updater.pubkey;
  assert.ok(stagingPubkey, "staging overlay must set plugins.updater.pubkey");
  assert.notEqual(
    stagingPubkey,
    productionPubkey,
    "staging must not share the production updater public key",
  );
  assert.match(
    Buffer.from(stagingPubkey, "base64").toString("utf8"),
    /minisign public key/,
  );
  assert.deepEqual(stagingOverlay.plugins.updater.endpoints, [
    "https://downloads.brightwave.io/tidebreak/staging/latest.json",
  ]);
  assert.deepEqual(stagingOverlay.plugins["deep-link"].desktop.schemes, [
    "tidebreak-staging",
  ]);
});

test("updater transition policy rejects unsafe ordering mutations", () => {
  // The shutdown-before-install boundary #1907 replaced. Kept as a fixture to
  // prove the policy no longer admits it now that the barrier is mandatory.
  const legacy = `
    async fn take_staged_and_restart() {
      app.state::<HostAccess>().shutdown().await;
      staged.update.install(&staged.bytes);
    }
  `;
  // This mirrors #1907: the generic helper owns the transition order, while
  // the Tauri call site supplies the concrete host and staged-update actions.
  const reversibleUpdater = `
    async fn take_staged_and_restart() {
      install_behind_broker_barrier(
        || host_access.quiesce_for_update(),
        || staged.update.install(&staged.bytes),
        || host_access.resume_after_failed_update(),
        || host_access.shutdown(),
      ).await;
    }
    async fn install_behind_broker_barrier(quiesce: Q, install: I, resume: R, shutdown: S) {
      quiesce().await?;
      match install() {
        Ok(()) => { shutdown().await; }
        Err(_) => { resume().await; }
      }
    }
  `;
  const reversibleBroker = `
    async fn admit(&self, command: BrokerCommand) {
      let admission = self.admission.lock().await;
      if *admission != BrokerAdmission::Running { return Err(()); }
      self.commands.try_send(command)?;
    }
    async fn run(&mut self) {
      match command {
        BrokerCommand::Quiesce { reply } => {
          self.ensure_session().await?;
          reply.send(());
        }
        BrokerCommand::ResumeAfterFailedUpdate { reply } => {
          self.allow_session_start = false;
          reply.send(());
        }
      }
    }
    async fn ensure_session(&mut self) {
      if !self.allow_session_start {
        return Err(BrokerClientError::UpdateRecovery);
      }
    }
  `;
  assert.ok(updaterTransitionIsSafe(reversibleUpdater, reversibleBroker));

  const unsafeCases = [
    [
      "legacy shutdown-before-install boundary without the broker barrier",
      legacy,
      "",
    ],
    [
      "legacy install before shutdown",
      `async fn take_staged_and_restart() { staged.update.install(&staged.bytes); app.state::<HostAccess>().shutdown().await; }`,
      "",
    ],
    [
      "legacy comment decoy",
      `async fn take_staged_and_restart() { staged.update.install(&staged.bytes); // app.state::<HostAccess>().shutdown().await\n }`,
      "",
    ],
    [
      "install before the broker barrier",
      reversibleUpdater.replace(
        "quiesce().await?;",
        "install(); quiesce().await?;",
      ),
      reversibleBroker,
    ],
    [
      "admission unlock before enqueue",
      reversibleUpdater,
      reversibleBroker.replace(
        "self.commands.try_send(command)?;",
        "drop(admission); self.commands.try_send(command)?;",
      ),
    ],
    [
      "quiesce acknowledgement before session pinning",
      reversibleUpdater,
      reversibleBroker.replace(
        "self.ensure_session().await?;\n          reply.send(());",
        "reply.send(());\n          self.ensure_session().await?;",
      ),
    ],
    [
      "resume acknowledgement before recovery gate",
      reversibleUpdater,
      reversibleBroker.replace(
        "self.allow_session_start = false;\n          reply.send(());",
        "reply.send(());\n          self.allow_session_start = false;",
      ),
    ],
    [
      "missing recovery gate",
      reversibleUpdater,
      reversibleBroker.replace(
        "if !self.allow_session_start {\n        return Err(BrokerClientError::UpdateRecovery);\n      }",
        "",
      ),
    ],
  ];
  for (const [name, updater, broker] of unsafeCases) {
    assert.equal(updaterTransitionIsSafe(updater, broker), false, name);
  }
});

test("the packaged desktop activates the signed updater feed", () => {
  assert.match(desktopCargo, /tauri-plugin-updater = "=[^"]+"/);
  assert.match(
    desktopHost,
    /\.plugin\(tauri_plugin_updater::Builder::new\(\)\.build\(\)\)/,
  );
  assert.match(desktopHost, /\.manage\(updater::UpdateManager::default\(\)\)/);
  assert.match(desktopHost, /updater::spawn_update_loop\(handle\.clone\(\)\)/);
  assert.match(desktopHost, /updater::desktop_update_state/);
  assert.match(desktopHost, /updater::check_for_update/);
  assert.match(desktopHost, /updater::restart_for_update/);
  assert.match(desktopUpdater, /updater\.check\(\)\.await/);
  assert.match(desktopUpdater, /update\.download\(/);
  assert.doesNotMatch(desktopUpdater, /download_and_install/);
  assert.ok(
    updaterTransitionIsSafe(desktopUpdater, desktopBroker),
    "updates must install behind the reversible broker barrier",
  );
  assert.match(desktopUpdater, /app\.restart\(\)/);
  // Production updates stay off debug builds. Current main enables macOS
  // only; the upcoming packaging change may also enable Windows and Linux
  // in the same cfg! or a sibling one.
  assert.match(
    desktopUpdater,
    /cfg!\(all\([\s\S]*not\(debug_assertions\),[\s\S]*target_os = "macos"[\s\S]*\)\)/,
  );
  assert.match(
    desktopUpdater,
    /cfg!\(all\(not\(debug_assertions\), target_os = "macos"\)\)/,
  );
  assert.match(
    desktopUpdater,
    /cfg!\(all\([\s\S]*not\(debug_assertions\),[\s\S]*target_os = "windows"[\s\S]*target_os = "linux"[\s\S]*\)\)/,
  );
  if (
    /target_os = "windows"/.test(desktopUpdater) ||
    /target_os = "linux"/.test(desktopUpdater)
  ) {
    assert.match(desktopUpdater, /target_os = "windows"/);
    assert.match(desktopUpdater, /target_os = "linux"/);
  }
  assert.match(
    desktopUpdater,
    /const UPDATE_CHECK_STARTUP_DELAY: Duration = Duration::from_secs\(15\)/,
  );
  assert.match(
    desktopUpdater,
    /const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs\(5 \* 60\)/,
  );
});
