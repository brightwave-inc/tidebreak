import { ContextUsageIndicator } from "tidebreak-desktop-ui";

// The numbers live in a hover tooltip; a static cell shows the ring at its
// composer size, so each cell pairs the ring with a caption for scale.

function Cell({
  caption,
  children,
}: {
  caption: string;
  children: React.ReactNode;
}) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: "0.6rem" }}>
      {children}
      <span
        style={{ fontSize: "0.75rem", color: "var(--muted-foreground)" }}
      >
        {caption}
      </span>
    </div>
  );
}

export function Normal() {
  return (
    <Cell caption="41% of 200k — Claude Opus 4.1">
      <ContextUsageIndicator
        usage={{
          input_tokens: 12_400,
          output_tokens: 3_180,
          cache_read_input_tokens: 62_000,
          cache_creation_input_tokens: 4_800,
        }}
        contextWindow={200_000}
        modelName="Claude Opus 4.1"
      />
    </Cell>
  );
}

export function Warning() {
  return (
    <Cell caption="81% of 200k — amber past 75%">
      <ContextUsageIndicator
        usage={{
          input_tokens: 41_000,
          output_tokens: 9_600,
          cache_read_input_tokens: 108_000,
          cache_creation_input_tokens: 3_400,
        }}
        contextWindow={200_000}
        modelName="Claude Opus 4.1"
      />
    </Cell>
  );
}

export function Critical() {
  return (
    <Cell caption="95% of 200k — destructive past 90%">
      <ContextUsageIndicator
        usage={{
          input_tokens: 52_000,
          output_tokens: 11_000,
          cache_read_input_tokens: 121_000,
          cache_creation_input_tokens: 6_000,
        }}
        contextWindow={200_000}
        modelName="Claude Opus 4.1"
      />
    </Cell>
  );
}

export function NoPublishedWindow() {
  return (
    <Cell caption="118,600 tokens; no published window — track only">
      <ContextUsageIndicator
        usage={{
          input_tokens: 34_600,
          output_tokens: 6_000,
          cache_read_input_tokens: 78_000,
          cache_creation_input_tokens: 0,
        }}
        contextWindow={undefined}
        modelName="ollama/qwen3-coder"
      />
    </Cell>
  );
}
