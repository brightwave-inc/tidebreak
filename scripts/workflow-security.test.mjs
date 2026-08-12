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
const denyConfig = readFileSync(repositoryFile("deny.toml"), "utf8");
const e2bPackage = JSON.parse(
  readFileSync(repositoryFile(".github", "e2b-cli", "package.json"), "utf8"),
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
  assert.match(draftJob, /id: release_drafter/);
  assert.match(
    draftJob,
    /RELEASE_TAG: v\$\{\{ steps\.release_drafter\.outputs\.resolved_version \}\}/,
  );
  assert.match(
    draftJob,
    /\{tag_name: \$tag, body: \$body\}/,
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
  assert.match(
    desktopCargo,
    /tidebreak-server = \{ path = "\.\.\/tidebreak-server" \}/,
  );
  assert.doesNotMatch(desktopCargo, /document-parsers/);
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
  assert.deepEqual(secretConsumers, ["publish-e2b-template.yml", "release.yml"]);

  const release = workflows["release.yml"];
  assert.match(release, /^on:\n  release:\n    types: \[published\]/m);
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

test("sandbox image publishing is tag-driven, immutable, and secret-free", () => {
  const publish = workflows["publish-sandbox-image.yml"];
  assert.ok(publish);

  // Version tags, main pushes (scoped in-workflow to the image inputs), the
  // weekly patch-flush schedule, and manual dispatch publish; a pull request
  // never can.
  assert.match(
    publish,
    /^on:\n  push:\n    tags: \["v\*"\]\n    branches: \[main\]\n  schedule:\n(?:    #.*\n)*    - cron: "43 4 \* \* 4"\n  workflow_dispatch:/m,
  );
  assert.doesNotMatch(publish, /^\s*pull_request(?:_target)?:/m);

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
  assert.match(publish, /^permissions:\n  contents: read$/m);
  assert.match(publish, /^    permissions:\n      contents: read\n      packages: write$/m);
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

test("sandbox images are scanned before push and the pin loop never touches main", () => {
  const publish = workflows["publish-sandbox-image.yml"];
  const build = workflowJob(publish, "build");
  const pin = workflowJob(publish, "pin");

  // The scanner is a checksum-pinned binary release, not a third-party
  // action, and it runs between build and push on both variants.
  assert.match(build, /trivy_\$\{TRIVY_VERSION\}_\$\{asset\}\.tar\.gz/);
  assert.match(build, /sha256sum --check/);
  assert.match(publish, /^  TRIVY_VERSION: \d+\.\d+\.\d+$/m);
  const scanIndex = build.indexOf("Scan both variants before push");
  assert.notEqual(scanIndex, -1);
  assert.ok(
    build.indexOf("Build both image variants") < scanIndex &&
      scanIndex < build.indexOf("Push per-architecture tags"),
    "the scan must gate the per-arch push",
  );

  // Policy: the run summary carries the full all-severities report, but the
  // publish fails only on fixable CRITICAL vulnerabilities or any secret. A
  // broader gate would drown in LibreOffice CVE noise and be ignored.
  assert.match(
    build,
    /trivy image --timeout 15m --scanners vuln,secret \\\n\s+--format table --output "\$report" "\$image"/,
  );
  assert.match(
    build,
    /trivy image --timeout 15m --scanners vuln \\\n\s+--severity CRITICAL --ignore-unfixed --exit-code 1 "\$image"/,
  );
  assert.match(
    build,
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

test("release builds use the trusted shared main cache scope", () => {
  const release = workflows["release.yml"];
  const dispatchJob = workflowJob(release, "dispatch");
  assert.match(release, /gh workflow run release\.yml/);
  assert.match(release, /--ref main/);
  assert.match(release, /actions: write/);
  assert.match(dispatchJob, /contents: read/);
  assert.match(
    dispatchJob,
    /node scripts\/check-release-tag\.mjs "\$RELEASE_TAG"/,
  );
  assert.ok(
    dispatchJob.indexOf("Reject an invalid published release tag") <
      dispatchJob.indexOf("gh workflow run release.yml"),
    "published release tags must be validated before dispatching a production build",
  );
  assert.match(
    release,
    /github\.event_name == 'workflow_dispatch' && github\.ref == 'refs\/heads\/main'/,
  );
  assert.match(
    release,
    /repos\/\$GITHUB_REPOSITORY\/releases\/tags\/\$RELEASE_TAG/,
  );
  assert.match(release, /ref: \$\{\{ needs\.validate\.outputs\.sha \}\}/);
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
        /shared-key: (?:macos-release-cargo-registry-v2|windows-release-cargo-registry-v1)-\$\{\{ hashFiles\('Cargo\.lock'\) \}\}/,
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
    assert.match(cacheStep, /target\/\$\{\{ matrix\.target \}\}\/release\/deps/);
    assert.match(
      cacheStep,
      /target\/\$\{\{ matrix\.target \}\}\/release\/tidebreak-desktop/,
    );
    assert.match(
      cacheStep,
      /target\/\$\{\{ matrix\.target \}\}\/release\/tidebreak-host-broker/,
    );
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
      /macos-release-target-v\d+-\$\{\{ matrix\.arch \}\}-/,
      "unsigned product caches should be preferred when available",
    );
    assert.doesNotMatch(
      restoreStep,
      /macos-release-(?:target|prepared)-v[12]-/,
      "older cache generations bake in runner-absolute checkout paths and must not be restored",
    );
  }

  // The credential-free jobs recompile from the tag or from `main`'s tip, so a
  // fallback that names only the architecture costs them compile time at
  // worst.
  for (const restoreStep of [releasePrepareCache, warmBuildCache]) {
    assert.match(
      restoreStep,
      /^\s*macos-release-target-v\d+-\$\{\{ matrix\.arch \}\}-$/m,
      "credential-free jobs may fall back to any warmed cache for this arch",
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
  for (const product of [
    /release\/tidebreak-desktop/,
    /release\/tidebreak-host-broker/,
    /libtidebreak_desktop_lib\./,
    /binaries\/tidebreak-host-broker-\$RELEASE_TARGET/,
  ]) {
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

  // The prerequisite compiles without any production credential and never
  // uploads what it built; only the cache carries its outputs forward.
  assert.match(prepareJob, /SCCACHE_GHA_RW_MODE: READ_ONLY/);
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
  // and restoring jobs; every restorable key pins at least the lockfile and
  // toolchain hashes.
  const cachedPaths = (step) =>
    step.match(/path: \|\n([\s\S]*?)\n\s+key:/)?.[1];
  assert.ok(cachedPaths(buildCacheRestore));
  assert.equal(cachedPaths(buildCacheRestore), cachedPaths(prepareCacheSave));
  const restorableKeys = buildCacheRestore
    .split("\n")
    .map((line) => line.trim().replace(/^key: /, ""))
    .filter((line) => line.startsWith("windows-release-"));
  assert.ok(restorableKeys.length >= 3);
  for (const key of restorableKeys) {
    assert.ok(
      key.includes("hashFiles('Cargo.lock',"),
      `restorable Windows cache key does not pin the source: ${key}`,
    );
  }

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

test("GitHub release downloads are copied from the hosted release", () => {
  const release = workflows["release.yml"];
  const attachJob = workflowJob(release, "attach_downloads");

  assert.match(attachJob, /needs: \[validate, publish\]/);
  assert.match(attachJob, /contents: write/);
  assert.doesNotMatch(attachJob, /^    environment:/m);
  assert.doesNotMatch(attachJob, /secrets\./);
  assert.doesNotMatch(attachJob, /APPLE_|TAURI_SIGNING|AWS_|DOWNLOADS_S3/);

  // Assets must be the CDN's own bytes, verified against the immutable
  // manifest, rather than a second copy built alongside the hosted release.
  assert.match(attachJob, /releases\/v\$TIDEBREAK_VERSION\/manifest\.json/);
  assert.match(attachJob, /sha256sum --check --strict/);
  assert.match(attachJob, /Tidebreak-macos-apple-silicon\.dmg/);
  assert.match(attachJob, /Tidebreak-windows-x86_64-setup\.exe/);
  assert.match(attachJob, /gh release upload "\$RELEASE_TAG"/);

  assert.match(
    readFileSync(repositoryFile("README.md"), "utf8"),
    /releases\/latest\/download\/Tidebreak-macos-apple-silicon\.dmg/,
    "the README download link must match the published asset name",
  );
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
  assert.match(
    desktopUpdater,
    /cfg!\(all\(not\(debug_assertions\), target_os = "macos"\)\)/,
  );
  assert.match(
    desktopUpdater,
    /const UPDATE_CHECK_STARTUP_DELAY: Duration = Duration::from_secs\(15\)/,
  );
  assert.match(
    desktopUpdater,
    /const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs\(5 \* 60\)/,
  );
});
