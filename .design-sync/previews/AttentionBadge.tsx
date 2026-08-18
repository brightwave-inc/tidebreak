import { AttentionBadge } from "tidebreak-desktop-ui";

const needsYou = {
  state: { type: "needs_you", prompt: "Needs you", source: "structured" },
  source: "structured",
};

const stalled = {
  state: { type: "stalled", idle_secs: 240 },
  source: "heuristic",
};

const doneUnreviewed = {
  state: { type: "done_unreviewed" },
  source: "lifecycle",
};

const fenced = {
  state: { type: "fenced", reason: { type: "orphan_alive" } },
  source: "lifecycle",
};

const pinned = {
  state: { type: "manual", note: "Pinned" },
  source: "user",
};

function LabeledRow({ label, children }: { label: string; children?: unknown }) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: "0.75rem" }}>
      <span
        className="text-muted-foreground text-sm"
        style={{ width: "10rem", flexShrink: 0 }}
      >
        {label}
      </span>
      {children as any}
    </div>
  );
}

export function BadgeStates() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "0.5rem" }}>
      <LabeledRow label="Needs you">
        <AttentionBadge attention={needsYou} />
      </LabeledRow>
      <LabeledRow label="Stalled 4m">
        <AttentionBadge attention={stalled} />
      </LabeledRow>
      <LabeledRow label="Fenced">
        <AttentionBadge attention={fenced} />
      </LabeledRow>
      <LabeledRow label="Pinned by you">
        <AttentionBadge attention={pinned} />
      </LabeledRow>
      <LabeledRow label="Done — unreviewed">
        <AttentionBadge attention={doneUnreviewed} />
      </LabeledRow>
      <LabeledRow label="Working (no badge)">
        <AttentionBadge attention={{ state: { type: "working" }, source: "lifecycle" }} />
        <span className="text-muted-foreground text-xs">renders nothing by design</span>
      </LabeledRow>
    </div>
  );
}

export function PromptedNeedsYou() {
  return (
    <AttentionBadge
      attention={{
        state: {
          type: "needs_you",
          prompt: "Approve `cargo publish`?",
          source: "structured",
        },
        source: "structured",
      }}
    />
  );
}

export function CompactDots() {
  const rows: [string, object][] = [
    ["Needs you", needsYou],
    ["Stalled 4m", stalled],
    ["Fenced", fenced],
    ["Done — unreviewed", doneUnreviewed],
  ];
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "0.5rem" }}>
      {rows.map(([label, attention]) => (
        <div
          key={label}
          style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}
        >
          <AttentionBadge attention={attention} compact />
          <span className="text-sm">{label}</span>
        </div>
      ))}
    </div>
  );
}
