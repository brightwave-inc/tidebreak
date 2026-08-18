import { Button, Spinner } from "tidebreak-desktop-ui";

export function Sizes() {
  return (
    <div style={{ display: "flex", gap: 20, alignItems: "center" }}>
      <div style={{ display: "flex", flexDirection: "column", gap: 6, alignItems: "center" }}>
        <Spinner className="size-3" aria-label="Working" />
        <span style={{ fontSize: 11, color: "var(--muted-foreground)" }}>size-3</span>
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 6, alignItems: "center" }}>
        <Spinner aria-label="Working" />
        <span style={{ fontSize: 11, color: "var(--muted-foreground)" }}>default</span>
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 6, alignItems: "center" }}>
        <Spinner className="size-6" aria-label="Working" />
        <span style={{ fontSize: 11, color: "var(--muted-foreground)" }}>size-6</span>
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 6, alignItems: "center" }}>
        <Spinner className="size-8" aria-label="Working" />
        <span style={{ fontSize: 11, color: "var(--muted-foreground)" }}>size-8</span>
      </div>
    </div>
  );
}

export function WithLabel() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <div style={{ display: "flex", gap: 8, alignItems: "center", fontSize: 14 }}>
        <Spinner aria-hidden />
        <span>Running checks on tb/fix-retry-test…</span>
      </div>
      <div style={{ display: "flex", gap: 8, alignItems: "center", fontSize: 14 }}>
        <Spinner aria-hidden />
        <span>Pushing branch to origin…</span>
      </div>
      <div
        style={{
          display: "flex",
          gap: 8,
          alignItems: "center",
          fontSize: 13,
          color: "var(--muted-foreground)",
        }}
      >
        <Spinner className="size-3" aria-hidden />
        <span>Indexing workspace</span>
      </div>
    </div>
  );
}

export function InButton() {
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
      <Button disabled>
        <Spinner className="size-4 text-primary-foreground" aria-hidden />
        Merging…
      </Button>
      <Button variant="outline" disabled>
        <Spinner className="size-4" aria-hidden />
        Creating workspace…
      </Button>
    </div>
  );
}
