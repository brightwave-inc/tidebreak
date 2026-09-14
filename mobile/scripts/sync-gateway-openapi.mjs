#!/usr/bin/env node
/**
 * Vendor the model-gateway admin (control-plane) OpenAPI document and generate
 * the TypeScript types the console screens consume.
 *
 * Two committed artifacts, one script:
 *
 *   schemas/gateway-admin-openapi.json  the snapshot, normalized for review
 *   src/generated/gatewayAdmin.ts       openapi-typescript output over it
 *
 * Modes, each with a package script:
 *
 *   sync-gateway-openapi       regenerate the types from the committed snapshot
 *   refresh-gateway-openapi    --fetch: refresh the snapshot from a running
 *                              gateway, then regenerate
 *   check-gateway-openapi      --check: write nothing; exit 1 if either artifact
 *                              is stale. This is what CI runs.
 *
 * Refreshing the snapshot (the canonical command) needs a gateway serving the
 * document. In a model-gateway checkout:
 *
 *   make dev-seed && make dev        # Postgres on 55433, gateway on 28081
 *
 * then, from this repository:
 *
 *   pnpm --dir mobile refresh-gateway-openapi
 *
 * Any reachable gateway will do; point at one by running this script directly
 * with `--fetch=https://gateway.example.com`. The route is unauthenticated on
 * purpose: it describes the shape of the API, not the contents of an
 * installation, so the snapshot carries no installation state, no server URLs,
 * and no credentials. Verify that when you refresh it.
 *
 * Snapshot-vs-live-gateway freshness is deliberately NOT checked by CI. The
 * snapshot is a pinned contract: it moves when the gateway's admin API changes
 * and we choose to adopt the change, not on every gateway deploy. CI only
 * enforces that the committed types match the committed snapshot, so a stale
 * snapshot is a visible, reviewable diff rather than a surprise at build time.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import openapiTS, { astToString } from "openapi-typescript";

const mobile = join(dirname(fileURLToPath(import.meta.url)), "..");
const snapshotPath = join(mobile, "schemas/gateway-admin-openapi.json");
const typesPath = join(mobile, "src/generated/gatewayAdmin.ts");
const defaultGateway = "http://127.0.0.1:28081";

// A bare `--` is how pnpm forwards flags; it reaches us verbatim, so drop it.
const args = process.argv.slice(2).filter((arg) => arg !== "--");
const check = args.includes("--check");
const fetchArg = args.find((arg) => arg === "--fetch" || arg.startsWith("--fetch="));
const unknown = args.filter((arg) => arg !== "--check" && arg !== fetchArg);
if (unknown.length > 0) {
  fail(`unknown argument(s): ${unknown.join(", ")}`);
}
if (check && fetchArg) {
  fail("--check verifies the committed artifacts; it cannot also --fetch");
}

/**
 * Sort object keys everywhere so the snapshot diff tracks the contract rather
 * than the serializer. Array order is meaningful in OpenAPI (enum variants,
 * `anyOf` branches, parameter order), so arrays are left exactly as served.
 */
function normalize(value) {
  if (Array.isArray(value)) return value.map(normalize);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, normalize(value[key])]),
    );
  }
  return value;
}

function fail(message) {
  console.error(`sync-gateway-openapi: ${message}`);
  process.exit(1);
}

async function loadSnapshot() {
  if (!fetchArg) {
    // Re-normalize what is committed: a hand-edited snapshot that skipped the
    // script shows up here as a diff instead of silently shaping the types.
    return normalize(JSON.parse(readFileSync(snapshotPath, "utf8")));
  }
  const base = fetchArg.includes("=") ? fetchArg.slice(fetchArg.indexOf("=") + 1) : defaultGateway;
  const url = new URL("/api/v1/openapi.json", base);
  const response = await fetch(url);
  if (!response.ok) {
    fail(`GET ${url} returned ${response.status} ${response.statusText}`);
  }
  return normalize(await response.json());
}

const snapshot = `${JSON.stringify(await loadSnapshot(), null, 2)}\n`;
// The generator reads the snapshot from disk, so a fetched document has to land
// before it runs. --check must not write, and there the committed file is
// already what we want the generator to read.
if (!check) writeFileSync(snapshotPath, snapshot);

const banner = [
  "// Generated from the model-gateway admin OpenAPI document. Do not edit.",
  "//",
  "// Source:     GET /api/v1/openapi.json (model-gateway control plane)",
  "// Snapshot:   mobile/schemas/gateway-admin-openapi.json",
  "// Regenerate: pnpm --dir mobile sync-gateway-openapi",
  "// Refresh the snapshot too: pnpm --dir mobile refresh-gateway-openapi",
  "",
  "",
].join("\n");

// openapi-typescript reads the snapshot from disk so `$ref` resolution has a
// base URL.
const types =
  banner +
  astToString(
    await openapiTS(pathToFileURL(snapshotPath), {
      alphabetize: true,
      excludeDeprecated: false,
    }),
  );

if (!check) {
  writeFileSync(typesPath, types);
  process.exit(0);
}

const stale = [
  [snapshotPath, snapshot],
  [typesPath, types],
].filter(([path, expected]) => readFileSync(path, "utf8") !== expected);

if (stale.length > 0) {
  const names = stale.map(([path]) => relative(mobile, path)).join(", ");
  fail(
    `${names} out of date. Run \`pnpm --dir mobile sync-gateway-openapi\` and commit the result.`,
  );
}
