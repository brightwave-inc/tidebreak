import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const policyTest = join(repositoryRoot, "scripts", "workflow-security.test.mjs");
const fixturePaths = [
  ".github/workflows",
  ".github/release-drafter.yml",
  ".github/e2b-cli/package.json",
  "crates/tidebreak-desktop/tauri.conf.json",
  "crates/tidebreak-desktop/tauri.staging.conf.json",
  "crates/tidebreak-desktop/Cargo.toml",
  "crates/tidebreak-desktop/src/lib.rs",
  "crates/tidebreak-desktop/src/updater.rs",
  "crates/tidebreak-desktop/src/broker.rs",
  "crates/tidebreak-desktop/scripts/prepare-sidecar.mjs",
  "deploy/self-host/Dockerfile",
  "deploy/self-host/Dockerfile.dockerignore",
  "crates/tidebreak-sandbox-agent/Dockerfile.dockerignore",
  "scripts/stage-self-host-build-context.sh",
  "deny.toml",
  "README.md",
];

function policyFixture() {
  const root = mkdtempSync(join(tmpdir(), "tidebreak-policy-mutation-"));
  for (const path of fixturePaths) {
    const source = join(repositoryRoot, path);
    const target = join(root, path);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(source, target, { recursive: true });
  }
  return root;
}

function edit(root, path, mutate) {
  const target = join(root, path);
  const before = readFileSync(target, "utf8");
  const after = mutate(before);
  assert.notEqual(after, before, `mutation did not change ${path}`);
  writeFileSync(target, after);
}

function editWorkflowJob(source, name, mutateJob) {
  const marker = `  ${name}:\n`;
  const start = source.indexOf(marker);
  assert.notEqual(start, -1, `missing workflow job: ${name}`);
  const remainder = source.slice(start + marker.length);
  const next = remainder.search(/^  [a-zA-Z0-9_-]+:\n/m);
  const end = next === -1 ? source.length : start + marker.length + next;
  const job = source.slice(start, end);
  const mutated = mutateJob(job);
  assert.notEqual(mutated, job, `mutation did not change job ${name}`);
  return source.slice(0, start) + mutated + source.slice(end);
}

function runPolicy(root) {
  const env = {
    ...process.env,
    TIDEBREAK_POLICY_ROOT: root,
    TIDEBREAK_SKIP_DOCKER_CONTEXT_PROBE: "1",
  };
  delete env.NODE_TEST_CONTEXT;
  // Running the file directly still executes node:test, while avoiding the
  // recursive test-runner guard when this mutation harness itself runs under
  // `node --test scripts/*.test.mjs`.
  return spawnSync(process.execPath, [policyTest], {
    cwd: repositoryRoot,
    encoding: "utf8",
    env,
  });
}

test("the mirrored workflow-security fixture passes before mutation", () => {
  const root = policyFixture();
  try {
    const result = runPolicy(root);
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

const mutations = [
  {
    name: "self-host mutable runtime packages",
    file: "deploy/self-host/Dockerfile",
    expected: "self-host runtime installs packages from an immutable Debian snapshot",
    mutate: (source) =>
      source
        .replace(
          /RUN rm -f \/etc\/apt\/sources\.list\.d\/debian\.sources[\s\S]*?&& apt-get -o Acquire::Check-Valid-Until=false update \\\n/,
          "RUN apt-get update \\\n",
        )
        .replace(/ca-certificates=[^\s\\]+/, "ca-certificates")
        .replace(/curl=[^\s\\]+/, "curl"),
  },
  {
    name: "sandbox image default-branch publication guard",
    file: ".github/workflows/publish-sandbox-image.yml",
    expected: "sandbox image publishing",
    mutate: (source) =>
      source.replace(
        /git merge-base --is-ancestor "\$SOURCE_SHA" "origin\/\$DEFAULT_BRANCH" \|\| \{[\s\S]*?\n\s+\}\n/,
        "",
      ),
  },
  {
    name: "sandbox image release-event trigger",
    file: ".github/workflows/publish-sandbox-image.yml",
    expected: "sandbox image publishing",
    mutate: (source) =>
      source.replace(
        /on:\n  release:\n    types: \[published\]\n  push:\n    branches: \[main\]/,
        'on:\n  push:\n    tags: ["v*"]\n    branches: [main]',
      ),
  },
  {
    name: "default-branch dispatch guard",
    file: ".github/workflows/publish-e2b-template.yml",
    expected: "production secrets remain isolated",
    mutate: (source) =>
      source.replace(
        /(workflow_dispatch\)\n)\s+\[\[ "\$SOURCE_REF" == "refs\/heads\/\$DEFAULT_BRANCH" &&\n\s+"\$SOURCE_REF_NAME" == "\$DEFAULT_BRANCH" \]\] \|\| \{[\s\S]*?\n\s+\}\n/,
        "$1              true\n",
      ),
  },
  {
    name: "release-tag syntax validation",
    file: ".github/workflows/publish-e2b-template.yml",
    expected: "production secrets remain isolated",
    mutate: (source) =>
      source.replace(
        '                node scripts/check-release-tag.mjs "$RELEASE_TAG"\n',
        "",
      ),
  },
  {
    name: "validated source-only checkout",
    file: ".github/workflows/publish-e2b-template.yml",
    expected: "production secrets remain isolated",
    mutate: (source) =>
      source.replace(
        "          ref: ${{ needs.resolve.outputs.source_sha }}",
        "          ref: ${{ github.sha }}",
      ),
  },
  {
    name: "E2B pin provenance source checkout",
    file: ".github/workflows/publish-e2b-template.yml",
    expected: "E2B template pin provenance",
    mutate: (source) =>
      source.replace(
        "          ref: ${{ needs.resolve.outputs.source_sha }}\n          path: .release-source\n          fetch-depth: 1\n          sparse-checkout: |\n            crates/tidebreak-sandbox-agent/e2b\n          sparse-checkout-cone-mode: false\n\n      - name: Point the client at the published template",
        "          ref: main\n          path: .release-source\n          fetch-depth: 1\n          sparse-checkout: |\n            crates/tidebreak-sandbox-agent/e2b\n          sparse-checkout-cone-mode: false\n\n      - name: Point the client at the published template",
      ),
  },
  {
    name: "source validation without credentials",
    file: ".github/workflows/publish-e2b-template.yml",
    expected: "production secrets remain isolated",
    mutate: (source) =>
      source.replace(
        "          GH_TOKEN: ${{ github.token }}\n",
        "          GH_TOKEN: ${{ github.token }}\n          E2B_API_KEY: ${{ secrets.E2B_API_KEY }}\n",
      ),
  },
  {
    name: "publish-job credential isolation",
    file: ".github/workflows/publish-e2b-template.yml",
    expected: "production secrets remain isolated",
    mutate: (source) =>
      source.replace(
        "    env:\n      ALIAS: ${{ needs.resolve.outputs.alias }}\n",
        "    env:\n      ALIAS: ${{ needs.resolve.outputs.alias }}\n      E2B_API_KEY: ${{ secrets.E2B_API_KEY }}\n",
      ),
  },
  {
    name: "frozen local E2B CLI installation",
    file: ".github/workflows/publish-e2b-template.yml",
    expected: "production secrets remain isolated",
    mutate: (source) =>
      source.replace(
        "pnpm --dir .github/e2b-cli install --frozen-lockfile --ignore-scripts",
        "npm install --global @e2b/cli@latest",
      ),
  },
  {
    name: "E2B tooling publication trigger",
    file: ".github/workflows/publish-e2b-template.yml",
    expected: "production secrets remain isolated",
    mutate: (source) => source.replace('      - ".github/e2b-cli/**"\n', ""),
  },
  {
    name: "E2B tooling PR scope",
    file: ".github/workflows/ci.yml",
    expected: "PR lanes are scope-gated",
    mutate: (source) => source.replace(".github/e2b-cli/*|", ""),
  },
  {
    name: "E2B tooling execution lane",
    file: ".github/workflows/ci.yml",
    expected: "PR lanes are scope-gated",
    mutate: (source) =>
      source.replace(/\n  e2b-cli:\n[\s\S]*?(?=\n  advisories:)/, ""),
  },
  {
    name: "documentation-site execution lane",
    file: ".github/workflows/ci.yml",
    expected: "PR lanes are scope-gated",
    mutate: (source) => source.replace(/\n  docs-site:\n[\s\S]*$/, ""),
  },
  {
    name: "locked cargo-deny graph",
    file: ".github/workflows/ci.yml",
    expected: "dependency policy covers",
    mutate: (source) => source.replace("--all-features --locked", "--all-features"),
  },
  {
    name: "Docker deny-all baseline",
    file: "deploy/self-host/Dockerfile.dockerignore",
    expected: "Docker context is allowlisted",
    mutate: (source) => source.replace("\n**\n", "\n"),
  },
  {
    name: "Docker arbitrary hidden-file denial",
    file: "deploy/self-host/Dockerfile.dockerignore",
    expected: "Docker context is allowlisted",
    mutate: (source) => source.replace("**/.*\n", ""),
  },
  {
    name: "Docker tracked-context staging",
    file: "scripts/stage-self-host-build-context.sh",
    expected: "self-host build context excludes",
    mutate: (source) => source.replace("git -C \"$root\" archive --format=tar \"$revision\"", "tar -cf - \"$root\""),
  },
  {
    name: "Docker private-key filename denial",
    file: "deploy/self-host/Dockerfile.dockerignore",
    expected: "Docker context is allowlisted",
    mutate: (source) => source.replace("**/id_*\n", ""),
  },
  {
    name: "source SBOM OIDC isolation",
    file: ".github/workflows/release.yml",
    expected: "source SBOM generation is isolated from production credentials",
    mutate: (source) =>
      editWorkflowJob(source, "source_sbom", (job) =>
        job.replace(
          "    permissions:\n      contents: read\n",
          "    permissions:\n      contents: read\n      id-token: write\n",
        ),
      ),
  },
  {
    name: "source SBOM production-environment isolation",
    file: ".github/workflows/release.yml",
    expected: "source SBOM generation is isolated from production credentials",
    mutate: (source) =>
      editWorkflowJob(source, "source_sbom", (job) =>
        job.replace(
          "    runs-on: ubuntu-latest\n",
          "    runs-on: ubuntu-latest\n    environment: desktop-production\n",
        ),
      ),
  },
  {
    name: "source SBOM deployment-variable isolation",
    file: ".github/workflows/release.yml",
    expected: "source SBOM generation is isolated from production credentials",
    mutate: (source) =>
      editWorkflowJob(source, "source_sbom", (job) =>
        job.replace(
          "    steps:\n",
          "    env:\n      AWS_RELEASE_ROLE_ARN: ${{ vars.AWS_RELEASE_ROLE_ARN }}\n    steps:\n",
        ),
      ),
  },
  {
    name: "source SBOM exact release checkout",
    file: ".github/workflows/release.yml",
    expected: "source SBOM generation is isolated from production credentials",
    mutate: (source) =>
      editWorkflowJob(source, "source_sbom", (job) =>
        job.replace(
          "          ref: ${{ needs.validate.outputs.sha }}\n",
          "          ref: ${{ github.sha }}\n",
        ),
      ),
  },
  {
    name: "source SBOM pinned artifact transfer",
    file: ".github/workflows/release.yml",
    expected: "source SBOM generation is isolated from production credentials",
    mutate: (source) =>
      editWorkflowJob(source, "source_sbom", (job) =>
        job.replace(
          /uses: actions\/upload-artifact@[0-9a-f]{40} # v7/,
          "uses: actions/upload-artifact@v7",
        ),
      ),
  },
  {
    name: "source SBOM pinned artifact download",
    file: ".github/workflows/release.yml",
    expected: "source SBOM generation is isolated from production credentials",
    mutate: (source) =>
      editWorkflowJob(source, "publish", (job) =>
        job.replace(
          /uses: actions\/download-artifact@[0-9a-f]{40} # v8\n        with:\n          name: tidebreak-source-sbom-/,
          "uses: actions/download-artifact@v8\n        with:\n          name: tidebreak-source-sbom-",
        ),
      ),
  },
  {
    name: "source SBOM is not an installer attestation",
    file: ".github/workflows/release.yml",
    expected: "public releases attest provenance without treating the source SBOM as an installer SBOM",
    mutate: (source) =>
      editWorkflowJob(source, "publish", (job) =>
        job.replace(
          "          subject-checksums: ${{ runner.temp }}/immutable-release-files.sha256\n",
          "          subject-checksums: ${{ runner.temp }}/immutable-release-files.sha256\n          sbom-path: dist/Tidebreak_${{ needs.validate.outputs.version }}_source.spdx.json\n",
        ),
      ),
  },
  {
    name: "signing job pnpm pin",
    file: ".github/workflows/release.yml",
    expected: "signing jobs do not run untrusted installers",
    mutate: (source) =>
      editWorkflowJob(source, "build_macos", (job) =>
        job.replace(/version: 10(?:\.18\.3)?\n/, "version: latest\n"),
      ),
  },
  {
    name: "signing job lockfile install",
    file: ".github/workflows/release.yml",
    expected: "signing jobs do not run untrusted installers",
    mutate: (source) =>
      editWorkflowJob(source, "build_macos", (job) =>
        job.replace(
          /      - name: Install UI dependencies\n        working-directory: crates\/tidebreak-desktop\/ui\n        run: pnpm install --frozen-lockfile(?: --ignore-scripts)?\n/,
          "",
        ),
      ),
  },
  {
    name: "signing job rust-cache writer",
    file: ".github/workflows/release.yml",
    expected: "signing jobs do not run untrusted installers",
    mutate: (source) =>
      editWorkflowJob(source, "build_macos", (job) =>
        job.replace(
          /save-if: (?:false|\$\{\{ matrix\.arch == 'aarch64' \}\})/,
          "save-if: true",
        ),
      ),
  },
  {
    name: "signing job floating pnpm",
    file: ".github/workflows/release.yml",
    expected: "signing jobs do not run untrusted installers",
    mutate: (source) =>
      editWorkflowJob(source, "build_macos", (job) =>
        job.replace(/version: 10\.18\.3\n/, "version: 10\n"),
      ),
  },
  {
    name: "signing job lifecycle scripts",
    file: ".github/workflows/release.yml",
    expected: "signing jobs do not run untrusted installers",
    mutate: (source) =>
      editWorkflowJob(source, "build_macos", (job) =>
        job.replace(
          "pnpm install --frozen-lockfile --ignore-scripts",
          "pnpm install --frozen-lockfile",
        ),
      ),
  },
  {
    name: "signing job installer order",
    file: ".github/workflows/release.yml",
    expected: "signing jobs do not run untrusted installers",
    mutate: (source) =>
      editWorkflowJob(source, "build_macos", (job) => {
        const install =
          "      - name: Install UI dependencies\n        working-directory: crates/tidebreak-desktop/ui\n        run: pnpm install --frozen-lockfile --ignore-scripts\n";
        assert.ok(job.includes(install));
        return job.replace(install, "").replace(
          "      - name: Validate production signing configuration\n",
          `      - name: Validate production signing configuration\n${install}`,
        );
      }),
  },
  {
    name: "production signing rust-cache save-if",
    file: ".github/workflows/release.yml",
    expected: "signing jobs do not run untrusted installers",
    mutate: (source) =>
      editWorkflowJob(source, "build_macos", (job) =>
        job.replace(
          "save-if: false",
          "save-if: ${{ matrix.arch == 'aarch64' }}",
        ),
      ),
  },
];

test("workflow-security controls fail closed under targeted mutations", async (t) => {
  for (const mutation of mutations) {
    await t.test(mutation.name, () => {
      const root = policyFixture();
      try {
        edit(root, mutation.file, mutation.mutate);
        const result = runPolicy(root);
        const output = `${result.stdout}\n${result.stderr}`;
        assert.notEqual(result.status, 0, `mutation passed unexpectedly:\n${output}`);
        assert.match(output, new RegExp(mutation.expected));
      } finally {
        rmSync(root, { recursive: true, force: true });
      }
    });
  }
});
