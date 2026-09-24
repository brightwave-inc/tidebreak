import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** The checkout this lane runs from. */
export const repositoryRoot = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
);

/**
 * The debug `tidebreak` binary with the self-host feature set.
 *
 * `TIDEBREAK_E2E_BINARY` names it when CI hands the lane a binary it already
 * built. Otherwise the path comes from Cargo's own target directory, so a
 * checkout with a custom `CARGO_TARGET_DIR` still finds its build.
 */
export function serverBinary(): string {
  const configured = process.env.TIDEBREAK_E2E_BINARY;
  const binary = configured
    ? resolve(configured)
    : join(cargoTargetDirectory(), "debug", "tidebreak");
  if (!existsSync(binary)) {
    throw new Error(
      `No debug server at ${binary}. Build it with ` +
        "`cargo build --locked -p tidebreak-cli --features tidebreak-server/postgres`, " +
        "or run scripts/e2e.sh.",
    );
  }
  return binary;
}

/** The built renderer the machine serves (decision 82). */
export function rendererBundle(): string {
  const bundle = resolve(
    process.env.TIDEBREAK_E2E_UI_DIST ??
      join(repositoryRoot, "crates", "tidebreak-desktop", "ui", "dist"),
  );
  if (!existsSync(join(bundle, "index.html"))) {
    throw new Error(
      `No renderer bundle at ${bundle}. Build it with ` +
        "`pnpm --dir crates/tidebreak-desktop/ui build`, or run scripts/e2e.sh.",
    );
  }
  return bundle;
}

function cargoTargetDirectory(): string {
  const metadata = execFileSync(
    "cargo",
    ["metadata", "--format-version", "1", "--no-deps"],
    { cwd: repositoryRoot, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  );
  return (JSON.parse(metadata) as { target_directory: string })
    .target_directory;
}
