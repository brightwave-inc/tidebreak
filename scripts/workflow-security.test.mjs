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
  assert.match(releaseDrafterConfig, /tag-template: "v\$RESOLVED_VERSION"/);
  assert.match(releaseDrafterConfig, /^tag-prefix: "v"$/m);

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
  // main: no platform-neutral lane may hide behind an opt-in label. The
  // `windows-ci` opt-in below is the one deliberate exception.
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
    /cargo test --workspace --exclude tidebreak-desktop --locked/,
  );
  assert.match(testJob, /attempts=3/);
  assert.match(testJob, /headless tests failed on attempt/);
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

test("engineering approval is a trusted base-branch check", () => {
  const workflow = workflows["engineering-approval.yml"];
  const approval = workflowJob(workflow, "approval");

  assert.match(
    workflow,
    /^on:\n(?:  #.*\n)+  pull_request_target:\n    types:\n/m,
  );
  assert.match(workflow, /^  pull_request_review:\n    types: /m);
  assert.match(workflow, /^  merge_group:\n/m);
  assert.match(workflow, /^permissions:\n  contents: read\n/m);
  assert.doesNotMatch(workflow, /secrets\./);
  assert.doesNotMatch(workflow, /^\s*pull_request:\n/m);
  assert.match(
    approval,
    /github\.event_name == 'pull_request_target' \|\| github\.event_name == 'pull_request_review' \|\| github\.event_name == 'merge_group'/,
  );
  assert.match(approval, /ref: \$\{\{ github\.event\.pull_request\.base\.sha \}\}/);
  assert.doesNotMatch(
    approval,
    /ref: \$\{\{ github\.event\.pull_request\.head\.sha \}\}/,
  );
  assert.match(
    approval,
    /PR_HEAD_SHA: \$\{\{ github\.event\.pull_request\.head\.sha \}\}/,
  );
  assert.match(approval, /CANONICAL_REPOSITORY: brightwave-inc\/tidebreak/);
  assert.match(approval, /node scripts\/check-engineering-approval\.mjs/);
  assert.match(
    approval,
    /echo "Merge groups reuse the pull request approval check\."/,
  );
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

test("native Windows CI is an explicit PR opt-in with a main backstop", () => {
  const windows = workflowJob(workflows["ci.yml"], "windows-check");
  const windowsCiGate =
    /github\.event_name != 'pull_request' \|\| contains\(github\.event\.pull_request\.labels\.\*\.name, 'windows-ci'\)/;

  assert.match(windows, windowsCiGate);
  assert.match(
    windows,
    /group: \$\{\{ github\.event_name == 'push' && 'windows-check-main' \|\| format\('windows-check-run-\{0\}', github\.run_id\) \}\}/,
  );
  assert.match(
    windows,
    /cancel-in-progress: \$\{\{ github\.event_name == 'push' \}\}/,
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
});

test("UI tests and production build each gate the UI lane", () => {
  const ci = workflows["ci.yml"];
  const ui = workflowJob(ci, "ui");

  assert.match(ui, /if:.*needs\.changes\.outputs\.ui == 'true'/);
  assert.match(ui, /run: pnpm install --frozen-lockfile/);
  // Sequential steps, one command each: a backgrounded `a & b & wait` swallows
  // the children's exit codes, so a failing test or build reported success
  // (#1376). Each step's status must reach the job directly.
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
    "release.yml",
    "staging-publish.yml",
  ]);
  for (const name of secretConsumers) {
    assert.ok(
      allowedSecretConsumers.has(name),
      `unexpected secret consumer: ${name}`,
    );
  }
  assert.ok(secretConsumers.includes("publish-e2b-template.yml"));
  assert.ok(secretConsumers.includes("release.yml"));

  const release = workflows["release.yml"];
  assert.doesNotMatch(release, /^  release:/m);
  assert.match(release, /^  workflow_dispatch:\n/m);
  assert.doesNotMatch(release, /^\s*pull_request(?:_target)?:/m);
  assert.match(release, /^permissions:\n  contents: read$/m);

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

test("release caches restore only credential-free compiler products", () => {
  const release = workflows["release.yml"];
  const cache = workflows["cache-macos.yml"];
  const prepareJob = workflowJob(release, "prepare_macos");
  const signedBuildJob = workflowJob(release, "build_macos");
  const releasePrepareCache = prepareJob.match(
    /- name: Restore unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const releasePrepareCacheSave = prepareJob.match(
    /- name: Save release-specific unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const releaseBuildCache = signedBuildJob.match(
    /- name: Restore unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const warmBuildCache = cache.match(
    /- name: Restore unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const warmBuildCacheSave = cache.match(
    /- name: Save unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(releasePrepareCache);
  assert.ok(releasePrepareCacheSave);
  assert.ok(releaseBuildCache);
  assert.ok(warmBuildCache);
  assert.ok(warmBuildCacheSave);
  const universalMacos = /--target universal-apple-darwin/.test(prepareJob);

  assert.match(prepareJob, /SCCACHE_GHA_ENABLED: "true"/);
  assert.match(prepareJob, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
  assert.match(prepareJob, /cache-targets: false/);
  assert.match(prepareJob, /--no-bundle --ci/);
  assert.match(prepareJob, /continue-on-error: true/);
  assert.match(releasePrepareCache, /actions\/cache\/restore@[0-9a-f]{40}/);
  assert.match(releasePrepareCacheSave, /actions\/cache\/save@[0-9a-f]{40}/);
  assert.doesNotMatch(prepareJob, /^    environment:/m);
  assert.doesNotMatch(prepareJob, /secrets\./);
  assert.doesNotMatch(
    prepareJob,
    /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_|actions\/upload-artifact/,
  );
  assert.ok(
    prepareJob.indexOf("Save release-specific unsigned Rust build cache") <
      prepareJob.indexOf("Require a successful unsigned compilation"),
    "release compiler outputs must be saved before a failed compile is reported",
  );

  assert.match(signedBuildJob, /SCCACHE_GHA_ENABLED: "true"/);
  assert.match(signedBuildJob, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
  assert.match(signedBuildJob, /cache-targets: false/);
  assert.match(releaseBuildCache, /actions\/cache\/restore@[0-9a-f]{40}/);
  assert.doesNotMatch(signedBuildJob, /actions\/cache\/save/);
  assert.ok(
    signedBuildJob.indexOf("Restore unsigned Rust build cache") <
      signedBuildJob.indexOf("Validate production signing configuration"),
    "the build cache must be restored before production secrets are loaded",
  );

  assert.match(cache, /SCCACHE_GHA_ENABLED: "true"/);
  assert.match(cache, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
  assert.match(cache, /cache-targets: false/);
  assert.match(warmBuildCache, /actions\/cache\/restore@[0-9a-f]{40}/);
  assert.match(cache, /actions\/cache\/save@[0-9a-f]{40}/);
  assert.ok(
    cache.indexOf("Save unsigned Rust build cache") <
      cache.indexOf("Require a successful cache-warm compilation"),
    "partial successful compiler outputs must be saved before a later failure is reported",
  );

  for (const cacheStep of [
    releasePrepareCache,
    releasePrepareCacheSave,
    releaseBuildCache,
    warmBuildCache,
    warmBuildCacheSave,
  ]) {
    assert.match(cacheStep, /target\/release\/\.fingerprint/);
    if (universalMacos) {
      for (const target of [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
      ]) {
        assert.match(cacheStep, new RegExp(`target/${target}/release/deps`));
        assert.match(
          cacheStep,
          new RegExp(`target/${target}/release/tidebreak-desktop`),
        );
        assert.match(
          cacheStep,
          new RegExp(`target/${target}/release/tidebreak-host-broker`),
        );
        assert.match(
          cacheStep,
          new RegExp(`binaries/tidebreak-host-broker-${target}`),
        );
      }
      assert.match(
        cacheStep,
        /binaries\/tidebreak-host-broker-universal-apple-darwin/,
      );
    } else {
      assert.match(cacheStep, /target\/\$\{\{ matrix\.target \}\}\/release\/deps/);
      assert.match(
        cacheStep,
        /target\/\$\{\{ matrix\.target \}\}\/release\/tidebreak-desktop/,
      );
      assert.match(
        cacheStep,
        /target\/\$\{\{ matrix\.target \}\}\/release\/tidebreak-host-broker/,
      );
    }
    assert.doesNotMatch(cacheStep, /pdfium/i);
    assert.doesNotMatch(cacheStep, /bundle|\.app|dmg|signature|keychain/i);
  }

  // `actions/cache` folds the path list into the cache version, so the signing
  // job can only reach an archive a writing job saved under the same list.
  // Keeping them identical is what makes the key ladder below, rather than a
  // silent total miss, the thing that governs what gets restored.
  const cachedPaths = (step) =>
    step.match(/path: \|\n([\s\S]*?)\n\s+key:/)?.[1];
  assert.ok(cachedPaths(releaseBuildCache));
  assert.equal(
    cachedPaths(releaseBuildCache),
    cachedPaths(releasePrepareCacheSave),
  );
  assert.equal(cachedPaths(releaseBuildCache), cachedPaths(warmBuildCacheSave));

  for (const restoreStep of [
    releasePrepareCache,
    releaseBuildCache,
    warmBuildCache,
  ]) {
    assert.match(
      restoreStep,
      universalMacos
        ? /macos-release-target-v5-universal-/
        : /macos-release-target-v4-\$\{\{ matrix\.arch \}\}-/,
      "unsigned product caches should be preferred when available",
    );
    assert.doesNotMatch(
      restoreStep,
      universalMacos
        ? /macos-release-(?:target|prepared)-v[1-4]-/
        : /macos-release-(?:target|prepared)-v[123]-/,
      "older cache generations bake in runner-absolute checkout paths and must not be restored",
    );
  }

  // The credential-free jobs recompile from the tag or from `main`'s tip, so a
  // fallback that names only the architecture costs them compile time at
  // worst.
  for (const restoreStep of [releasePrepareCache, warmBuildCache]) {
    assert.match(
      restoreStep,
      universalMacos
        ? /^\s*macos-release-target-v5-universal-$/m
        : /^\s*macos-release-target-v4-\$\{\{ matrix\.arch \}\}-$/m,
      universalMacos
        ? "credential-free jobs may fall back to any warmed universal cache"
        : "credential-free jobs may fall back to any warmed cache for this arch",
    );
  }

  // The signing job signs what it restores, so every key it can hit must pin
  // the source identity of the archive: the release commit SHA, or at minimum
  // the Cargo.lock and toolchain hashes. An arch-only fallback would restore
  // an archive warmed from an unrelated `main` commit and sign binaries that
  // do not correspond to the released tag.
  const signingCacheKeys = releaseBuildCache
    .split("\n")
    .map((line) => line.trim().replace(/^key: /, ""))
    .filter((line) => line.startsWith("macos-release-"));
  assert.ok(signingCacheKeys.length >= 4);
  for (const key of signingCacheKeys) {
    assert.ok(
      key.includes("hashFiles('Cargo.lock',"),
      `restorable signing-job cache key does not pin the source: ${key}`,
    );
  }

  // Whatever a restore extracts, the products that actually get signed are
  // relinked from the tag's own sources.
  const discardProducts = signedBuildJob.match(
    /- name: Discard restored product binaries[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(discardProducts);
  const discardedProducts = universalMacos
    ? [
        /aarch64-apple-darwin,x86_64-apple-darwin,universal-apple-darwin.*release\/tidebreak-desktop/,
        /aarch64-apple-darwin,x86_64-apple-darwin.*release\/tidebreak-host-broker/,
        /libtidebreak_desktop_lib\./,
        /binaries\/tidebreak-host-broker-\{aarch64-apple-darwin,x86_64-apple-darwin\}/,
        /binaries\/tidebreak-host-broker-universal-apple-darwin/,
      ]
    : [
        /release\/tidebreak-desktop/,
        /release\/tidebreak-host-broker/,
        /libtidebreak_desktop_lib\./,
        /binaries\/tidebreak-host-broker-\$RELEASE_TARGET/,
      ];
  for (const product of discardedProducts) {
    assert.match(discardProducts, product);
  }
  assert.ok(
    signedBuildJob.indexOf("Discard restored product binaries") >
      signedBuildJob.indexOf("Restore unsigned Rust build cache") &&
      signedBuildJob.indexOf("Discard restored product binaries") <
        signedBuildJob.indexOf("Build, sign, and notarize the Tauri app"),
    "restored product binaries must be discarded before the signed build",
  );
});

test("Windows release jobs mirror the credential-free prepare/build split", () => {
  const release = workflows["release.yml"];
  const prepareJob = workflowJob(release, "prepare_windows");
  const buildJob = workflowJob(release, "build_windows");
  const prepareCacheSave = prepareJob.match(
    /- name: Save release-specific unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  const buildCacheRestore = buildJob.match(
    /- name: Restore unsigned Rust build cache[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(prepareCacheSave);
  assert.ok(buildCacheRestore);
  for (const job of [prepareJob, buildJob]) {
    assert.match(job, /runs-on: \$\{\{ matrix\.runner \}\}/);
    assert.match(job, /target: x86_64-pc-windows-msvc/);
    assert.match(job, /runner: windows-latest/);
    assert.match(job, /target: aarch64-pc-windows-msvc/);
    assert.match(job, /runner: windows-11-arm/);
    // whisper.cpp refuses MSVC on ARM. Force Ninja + clang-cl only on the
    // ARM matrix entry so x86_64 keeps the Visual Studio generator.
    assert.match(job, /if: \$\{\{ matrix\.arch == 'aarch64' \}\}/);
    assert.match(job, /CMAKE_GENERATOR=Ninja/);
    assert.match(job, /CMAKE_C_COMPILER=/);
    assert.match(job, /CMAKE_CXX_COMPILER=/);
    assert.match(job, /CMAKE_ASM_COMPILER=/);
    assert.match(job, /echo "CC=\$clang_cl"/);
    assert.match(job, /echo "CXX=\$clang_cl"/);
    assert.match(job, /CXXFLAGS=\/EHsc/);
    assert.match(job, /clang-cl/);
    assert.match(job, /command -v ninja/);
  }
  assert.ok(
    prepareJob.indexOf("Use clang-cl for Windows ARM native code") <
      prepareJob.indexOf("Compile without production credentials or bundles"),
    "Windows ARM clang-cl must be configured before the unsigned compile",
  );
  assert.ok(
    buildJob.indexOf("Use clang-cl for Windows ARM native code") <
      buildJob.indexOf("Build the Tauri app without Windows code signing"),
    "Windows ARM clang-cl must be configured before the packaged build",
  );

  // The prerequisite compiles without any production credential and never
  // uploads what it built; only the cache carries its outputs forward.
  assert.match(prepareJob, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
  assert.match(prepareJob, /version: 10\.18\.3/);
  assert.match(prepareJob, /pnpm install --frozen-lockfile --ignore-scripts/);
  assert.match(
    prepareJob,
    /node scripts\/generate-third-party-notices\.mjs --check/,
  );
  assert.match(prepareJob, /--no-bundle --ci/);
  assert.match(prepareJob, /continue-on-error: true/);
  assert.doesNotMatch(prepareJob, /^    environment:/m);
  assert.doesNotMatch(prepareJob, /secrets\./);
  assert.doesNotMatch(
    prepareJob,
    /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_|actions\/upload-artifact/,
  );
  assert.ok(
    prepareJob.indexOf("Save release-specific unsigned Rust build cache") <
      prepareJob.indexOf("Require a successful unsigned compilation"),
    "release compiler outputs must be saved before a failed compile is reported",
  );

  // The packaging job is restore-only and loads the updater key only after
  // the credential-free cache restore.
  assert.doesNotMatch(buildJob, /actions\/cache\/save/);
  assert.ok(
    buildJob.indexOf("Restore unsigned Rust build cache") <
      buildJob.indexOf("Validate updater signing configuration"),
    "the build cache must be restored before the updater secret is loaded",
  );

  // Identical path lists keep the cache version shared between the writing
  // and restoring jobs. The credentialed job accepts only the exact cache for
  // this release SHA and version; broader warm-cache fallback stays confined
  // to the credential-free prerequisite.
  const cachedPaths = (step) =>
    step.match(/path: \|\n([\s\S]*?)\n\s+key:/)?.[1];
  assert.ok(cachedPaths(buildCacheRestore));
  assert.equal(cachedPaths(buildCacheRestore), cachedPaths(prepareCacheSave));
  assert.match(
    buildCacheRestore,
    /key: windows-release-prepared-v1-\$\{\{ matrix\.arch \}\}-\$\{\{ hashFiles\('Cargo\.lock', 'rust-toolchain\.toml'\) \}\}-\$\{\{ needs\.validate\.outputs\.sha \}\}-\$\{\{ needs\.validate\.outputs\.version \}\}/,
  );
  assert.doesNotMatch(buildCacheRestore, /restore-keys:/);

  // Whatever a restore extracts, the products that get packaged are relinked
  // from the tag's own sources.
  const discardProducts = buildJob.match(
    /- name: Discard restored product binaries[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(discardProducts);
  for (const product of [
    /release\/tidebreak-desktop\.exe/,
    /release\/tidebreak-host-broker\.exe/,
    /tidebreak_desktop_lib\./,
    /binaries\/tidebreak-host-broker-\$RELEASE_TARGET\.exe/,
  ]) {
    assert.match(discardProducts, product);
  }
  assert.ok(
    buildJob.indexOf("Discard restored product binaries") >
      buildJob.indexOf("Restore unsigned Rust build cache") &&
      buildJob.indexOf("Discard restored product binaries") <
        buildJob.indexOf("Build the Tauri app without Windows code signing"),
    "restored product binaries must be discarded before the packaged build",
  );

  // v1 ships unsigned Windows artifacts: no Authenticode configuration may
  // creep in, no Apple credential reaches the Windows jobs, and the updater
  // private key stays out of the Tauri build step.
  assert.doesNotMatch(release, /certificateThumbprint|signCommand/);
  assert.doesNotMatch(buildJob, /APPLE_|AWS_|DOWNLOADS_/);
  const buildStep = buildJob.match(
    /- name: Build the Tauri app without Windows code signing[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(buildStep);
  assert.doesNotMatch(buildStep, /TAURI_SIGNING_PRIVATE_KEY/);
});

test("Linux packaging writes no shared cache before loading updater material", () => {
  const release = workflows["release.yml"];
  assert.doesNotMatch(release, /^  prepare_linux:/m);
  const buildJob = workflowJob(release, "build_linux");
  assert.match(buildJob, /needs: \[validate, inspect_hosted\]/);
  // Current workflow pins the x86_64 job to ubuntu-22.04. The upcoming ARM64
  // matrix keeps that runner and selects it through ${{ matrix.runner }}.
  assert.match(buildJob, /ubuntu-22\.04/);
  assert.match(
    buildJob,
    /runs-on: (?:ubuntu-22\.04|\$\{\{ matrix\.runner \}\})/,
  );
  if (/runner: ubuntu-22\.04-arm/.test(buildJob)) {
    assert.match(buildJob, /target: x86_64-unknown-linux-gnu/);
    assert.match(buildJob, /target: aarch64-unknown-linux-gnu/);
  }
  assert.match(buildJob, /runs-on: \$\{\{ matrix\.runner \}\}/);
  assert.match(buildJob, /target: x86_64-unknown-linux-gnu/);
  assert.match(buildJob, /runner: ubuntu-22\.04/);
  assert.match(buildJob, /target: aarch64-unknown-linux-gnu/);
  assert.match(buildJob, /runner: ubuntu-22\.04-arm/);
  assert.match(buildJob, /libwebkit2gtk-4\.1-dev/);
  assert.match(
    buildJob,
    /xdg-utils/,
    "AppImage packaging must install the xdg-mime provider on every runner",
  );
  assert.match(
    buildJob,
    /scripts\/install-linux-apt-packages\.sh/,
    "Linux packaging must pin apt to a public Ubuntu archive instead of a hanging Azure mirror",
  );
  const linuxDeps = buildJob.match(
    /- name: Install Linux packaging dependencies[\s\S]*?(?=\n\s+- (?:name:|uses:))/,
  )?.[0];
  assert.ok(linuxDeps, "missing Linux packaging dependency step");
  assert.match(linuxDeps, /timeout-minutes: 8/);
  assert.doesNotMatch(linuxDeps, /sudo apt-get update/);
  assert.match(buildJob, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
  assert.match(buildJob, /version: 10\.18\.3/);
  assert.match(buildJob, /pnpm install --frozen-lockfile --ignore-scripts/);
  assert.match(
    buildJob,
    /node scripts\/generate-third-party-notices\.mjs --check/,
  );
  assert.doesNotMatch(buildJob, /actions\/cache\/save/);
  assert.doesNotMatch(buildJob, /actions\/cache\/restore/);
  assert.doesNotMatch(buildJob, /cache: pnpm/);
  const rustCache = cargoDownloadCache(buildJob);
  assert.ok(rustCache);
  assert.match(rustCache, /save-if: false/);

  assert.match(buildJob, /--bundles appimage,deb/);
  assert.match(buildJob, /category: "Productivity"/);
  assert.doesNotMatch(buildJob, /category: "Office"/);
  assert.match(buildJob, /\.AppImage/);
  assert.match(buildJob, /\.deb/);
  assert.match(buildJob, /tauri signer sign "\$appimage_path"/);
  assert.match(buildJob, /tauri signer sign "\$deb_path"/);
  assert.match(buildJob, /\$\{base_name\}\.deb\.sig/);
  const buildStep = buildJob.match(
    /- name: Build the Tauri Linux packages[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(buildStep);
  assert.doesNotMatch(buildStep, /TAURI_SIGNING_PRIVATE_KEY/);
  assert.ok(
    buildJob.indexOf("Build the Tauri Linux packages") <
      buildJob.indexOf("Verify and collect Linux artifacts"),
    "Linux packages must be built before updater signing material is loaded",
  );
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
  const outputDirectoryIndex = sbomJob.indexOf(
    "- name: Create the source SBOM output directory",
  );
  const generatorIndex = sbomJob.indexOf(
    "- name: Generate the source-scoped release SBOM",
  );
  assert.ok(outputDirectoryIndex !== -1 && generatorIndex !== -1);
  assert.ok(outputDirectoryIndex < generatorIndex);
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

  // A fresh release attaches the exact verified Actions artifacts before
  // publication. A published retry verifies those assets and can reconstruct
  // an incomplete hosted publication without rebuilding signed packages.
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
  // Current publication lists the x86_64 Debian signature literally. The
  // upcoming ARM64 graph may keep that literal or expand it through ${arch}.
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

test("the updater private key is isolated from compilation", () => {
  const release = workflows["release.yml"];
  const buildStep = release.match(
    /- name: Build, sign, and notarize the Tauri app[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(buildStep);
  assert.doesNotMatch(buildStep, /TAURI_SIGNING_PRIVATE_KEY/);
  assert.doesNotMatch(release, /createUpdaterArtifacts/);
  assert.match(release, /tauri signer sign "\$updater_path"/);
  assert.doesNotMatch(release, /cargo tauri signer sign/);
});

test("signing jobs do not run untrusted installers next to Apple or Tauri material", () => {
  for (const { file, name, job, validate } of desktopSigningJobs()) {
    const label = `${name} (${file})`;
    assert.match(
      job,
      /uses: pnpm\/action-setup@[0-9a-f]{40}/,
      `${label} must set up pnpm`,
    );
    assert.match(
      job,
      /version: 10\.18\.3\n/,
      `${label} must pin pnpm 10.18.3`,
    );
    assert.match(
      job,
      /pnpm install --frozen-lockfile --ignore-scripts\n/,
      `${label} must install UI deps without lifecycle scripts`,
    );
    assert.match(
      job,
      /mozilla-actions\/sccache-action@[0-9a-f]{40}/,
      `${label} must set up sccache`,
    );
    assert.match(
      job,
      /Swatinem\/rust-cache@[0-9a-f]{40}/,
      `${label} must restore the Cargo download cache`,
    );
    assert.match(
      job,
      /actions\/setup-node@[0-9a-f]{40}/,
      `${label} must set up Node`,
    );

    const rustCache = cargoDownloadCache(job);
    assert.ok(rustCache, `${label} missing Cargo download cache`);
    assert.match(
      rustCache,
      /save-if: false/,
      `${label} rust-cache must be restore-only`,
    );

    const secretsAt = firstSigningMaterialIndex(job, validate);
    assert.notEqual(secretsAt, -1, `${label} must still load signing material`);

    const installerIndexes = [
      job.search(/mozilla-actions\/sccache-action@[0-9a-f]{40}/),
      job.search(/Swatinem\/rust-cache@[0-9a-f]{40}/),
      job.search(/pnpm\/action-setup@[0-9a-f]{40}/),
      job.search(/actions\/setup-node@[0-9a-f]{40}/),
      job.search(/pnpm install --frozen-lockfile --ignore-scripts\n/),
    ];
    assert.ok(
      installerIndexes.every((index) => index !== -1),
      `${label} is missing an installer step`,
    );
    assert.ok(
      installerIndexes.every((index) => index < secretsAt),
      `${label} must finish installers before signing material is loaded`,
    );
  }
});

test("macOS disk images are explicitly notarized and stapled", () => {
  const release = workflows["release.yml"];
  const dmgNotarization = release.match(
    /- name: Notarize and staple the DMG[\s\S]*?(?=\n\s+- name:)/,
  )?.[0];
  assert.ok(dmgNotarization);
  assert.match(dmgNotarization, /xcrun notarytool submit "\$dmg_path"/);
  assert.match(dmgNotarization, /--key "\$APPLE_API_KEY_PATH"/);
  assert.match(dmgNotarization, /xcrun stapler staple "\$dmg_path"/);
  assert.match(dmgNotarization, /xcrun stapler validate "\$dmg_path"/);
  assert.ok(
    release.indexOf("Build, sign, and notarize the Tauri app") <
      release.indexOf("Notarize and staple the DMG"),
  );
  assert.ok(
    release.indexOf("Notarize and staple the DMG") <
      release.indexOf("Verify and collect signed artifacts"),
  );
});

test("universal macOS release and staging packages contain both slices", () => {
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

  for (const job of [releaseBuild, stagingBuild, warm]) {
    assert.match(job, /rustup target add aarch64-apple-darwin x86_64-apple-darwin/);
    assert.match(job, /--target universal-apple-darwin/);
  }

  for (const job of [releaseBuild, stagingBuild]) {
    assert.match(job, /timeout-minutes: 90/);
    assert.match(job, /lipo -archs "\$app_path\/Contents\/MacOS\/\$executable"/);
    assert.match(job, /\$binary_arches" = \*arm64\*/);
    assert.match(job, /\$binary_arches" = \*x86_64\*/);
    assert.match(job, /Contents\/MacOS\/tidebreak-host-broker/);
    assert.match(job, /sidecar_arches="\$\(lipo -archs "\$sidecar"\)"/);
  }
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
  assert.match(staging, /^on:\n  push:\n    branches: \[main\]/m);
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
