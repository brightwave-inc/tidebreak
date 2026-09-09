import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const helperName = "tidebreak-cu-helper";

export function stageComputerUseHelper(
  { desktopDir, workspaceDir, targetRoot, triple, release },
  { run = execFileSync, platform = process.platform, env = process.env } = {},
) {
  if (!triple.endsWith("apple-darwin")) return;
  if (platform !== "darwin") {
    throw new Error("Build the macOS computer-use helper on macOS with Xcode installed");
  }
  const targets = triple === "universal-apple-darwin"
    ? ["aarch64-apple-darwin", "x86_64-apple-darwin"] : [triple];
  const binaryDir = join(desktopDir, "binaries");
  const resourceDir = join(desktopDir, "resources", "host-broker");
  mkdirSync(binaryDir, { recursive: true });
  mkdirSync(resourceDir, { recursive: true });
  const staged = [];
  for (const target of targets) {
    const arch = { "aarch64-apple-darwin": "arm64", "x86_64-apple-darwin": "x86_64" }[target];
    if (!arch) throw new Error(`Unsupported computer-use helper target: ${target}`);
    const args = [
      "build", "--package-path", join(workspaceDir, "crates", "tidebreak-host-broker", "helper"),
      "--scratch-path", join(targetRoot, "computer-use-helper", arch),
      "--configuration", release ? "release" : "debug", "--arch", arch,
    ];
    run("swift", [...args, "--product", helperName], { cwd: workspaceDir, stdio: "inherit" });
    const output = run("swift", [...args, "--show-bin-path"], { cwd: workspaceDir, encoding: "utf8" }).trim();
    if (!output) throw new Error("Swift did not report the computer-use helper output directory");
    const destination = join(binaryDir, `${helperName}-${target}`);
    copyFileSync(join(output, helperName), destination);
    chmodSync(destination, 0o755);
    staged.push(destination);
  }
  let binary = staged[0];
  if (targets.length > 1) {
    binary = join(binaryDir, `${helperName}-${triple}`);
    run("lipo", ["-create", ...staged, "-output", binary], { stdio: "inherit" });
    chmodSync(binary, 0o755);
  }
  const resource = join(resourceDir, helperName);
  copyFileSync(binary, resource);
  chmodSync(resource, 0o755);
  // Release CI signs after combining architectures and restoring the resource.
  // Dev builds use a stable identity so macOS grants can survive recompilation.
  if (!release) {
    let identity = env.TIDEBREAK_DEV_SIGNING_IDENTITY;
    if (identity === undefined) {
      const identities = run("security", ["find-identity", "-v", "-p", "codesigning"], { encoding: "utf8" });
      identity = identities.match(/"(Apple Development: [^"]+)"/)?.[1]
        ?? identities.match(/"(Developer ID Application: [^"]+)"/)?.[1];
    }
    if (identity) {
      run("codesign", ["--force", "--sign", identity, "--identifier", "io.brightwave.tidebreak.cu-helper.dev", resource], { stdio: "inherit" });
      run("codesign", ["--verify", "--strict", resource], { stdio: "inherit" });
    } else {
      console.warn("Computer-use helper has no stable development signing identity; macOS may ask for permissions after each rebuild.");
    }
  }
  return resource;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const desktopDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const workspaceDir = resolve(desktopDir, "../..");
  const triple = process.env.TAURI_ENV_TARGET_TRIPLE?.trim()
    || execFileSync("rustc", ["--print", "host-tuple"], { encoding: "utf8" }).trim();
  stageComputerUseHelper({
    desktopDir, workspaceDir, triple,
    targetRoot: process.env.CARGO_TARGET_DIR ? resolve(workspaceDir, process.env.CARGO_TARGET_DIR) : join(workspaceDir, "target"),
    release: process.argv.includes("--release"),
  });
}
