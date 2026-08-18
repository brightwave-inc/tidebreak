import { ToolStatusIcon } from "tidebreak-desktop-ui";

const TONES = [
  "running",
  "waiting_approval",
  "completed",
  "cancelled",
  "failed",
  "unknown",
] as const;

export function Tones() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      {TONES.map((tone) => (
        <div
          key={tone}
          style={{ display: "flex", alignItems: "center", gap: 10 }}
        >
          <span
            style={{
              display: "inline-flex",
              width: 20,
              justifyContent: "center",
            }}
          >
            <ToolStatusIcon tone={tone} />
          </span>
          <code style={{ fontSize: 11 }}>{tone}</code>
          {tone === "completed" && (
            <span className="text-muted-foreground" style={{ fontSize: 11 }}>
              renders nothing by design — success stays quiet
            </span>
          )}
        </div>
      ))}
    </div>
  );
}
