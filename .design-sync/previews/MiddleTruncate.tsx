import { MiddleTruncate } from "tidebreak-desktop-ui";

const path = "crates/tidebreak-desktop/ui/src/code/CodeSessionReducer.test.ts";
const branch = "tidebreak/terminal-theme-tokens-and-keyboard-resize";

export function Widths() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8, fontFamily: "var(--mono)", fontSize: 12 }}>
      {[360, 260, 180].map((w) => (
        <div key={w} style={{ width: w, border: "1px solid var(--border)", borderRadius: 6, padding: "4px 8px" }}>
          <MiddleTruncate text={path} />
        </div>
      ))}
    </div>
  );
}

export function BranchName() {
  return (
    <div style={{ width: 220, fontFamily: "var(--mono)", fontSize: 12 }}>
      <MiddleTruncate text={branch} />
    </div>
  );
}
