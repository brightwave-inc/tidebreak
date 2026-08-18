import { CodeToolCard } from "tidebreak-desktop-ui";

const checkPreview = `    Checking tidebreak-core v0.9.2 (/work/tidebreak/crates/tidebreak-core)
    Checking tidebreak-store v0.9.2 (/work/tidebreak/crates/tidebreak-store)
    Checking tidebreak-server v0.9.2 (/work/tidebreak/crates/tidebreak-server)`;

const failPreview = `error[E0308]: mismatched types
   --> crates/tidebreak-server/src/turns.rs:214:9
    |
214 |         attention
    |         ^^^^^^^^^ expected \`Attention\`, found \`AttentionState\`

error: could not compile \`tidebreak-server\` (lib) due to 1 previous error`;

export function RunningCommand() {
  return (
    <CodeToolCard
      name="shell"
      detail={{
        kind: "command",
        cmd: "cargo check --workspace",
        cwd: "/work/tidebreak",
      }}
      status="running"
      preview={checkPreview}
    />
  );
}

export function SucceededRead() {
  return (
    <CodeToolCard
      name="read_file"
      detail={{ kind: "file_read", path: "crates/tidebreak-server/src/turns.rs" }}
      status="succeeded"
      preview=""
    />
  );
}

export function FailedCommand() {
  return (
    <CodeToolCard
      name="shell"
      detail={{
        kind: "command",
        cmd: "cargo test -p tidebreak-server",
        cwd: "/work/tidebreak",
      }}
      status="failed"
      preview={failPreview}
    />
  );
}

export function DeniedEdit() {
  return (
    <CodeToolCard
      name="edit_file"
      detail={{ kind: "file_edit", path: ".github/workflows/ci.yml" }}
      status="denied"
      preview=""
    />
  );
}
