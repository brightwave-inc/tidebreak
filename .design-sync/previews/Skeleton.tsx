import { Skeleton } from "tidebreak-desktop-ui";

export function TranscriptLoading() {
  return (
    <div
      style={{ display: "flex", flexDirection: "column", gap: 16, width: 420 }}
      aria-hidden="true"
    >
      {[0, 1].map((row) => (
        <div key={row} style={{ display: "flex", flexDirection: "column", gap: 16 }}>
          <Skeleton className="h-9 w-1/2 self-start rounded-xl" />
          <div style={{ display: "flex", flexDirection: "column", gap: 10, padding: "8px 0" }}>
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-5/6" />
            <Skeleton className="h-3 w-1/3" />
          </div>
        </div>
      ))}
    </div>
  );
}

export function WorkspaceListLoading() {
  return (
    <div
      style={{ display: "flex", flexDirection: "column", gap: 14, width: 380 }}
      aria-hidden="true"
    >
      {[0, 1, 2, 3].map((row) => (
        <div key={row} style={{ display: "flex", gap: 10, alignItems: "center" }}>
          <Skeleton className="size-8 rounded-full" />
          <div style={{ display: "flex", flexDirection: "column", gap: 6, flex: 1 }}>
            <Skeleton className="h-3.5 w-3/5" />
            <Skeleton className="h-3 w-2/5" />
          </div>
          <Skeleton className="h-5 w-16 rounded-full" />
        </div>
      ))}
    </div>
  );
}

export function PanelLoading() {
  return (
    <div
      style={{
        width: 380,
        border: "1px solid var(--border)",
        borderRadius: 8,
        overflow: "hidden",
      }}
      aria-hidden="true"
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "10px 12px",
          borderBottom: "1px solid var(--border)",
        }}
      >
        <Skeleton className="h-4 w-40" />
        <div style={{ flex: 1 }} />
        <Skeleton className="size-6 rounded-md" />
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 10, padding: 12 }}>
        <Skeleton className="h-3 w-full" />
        <Skeleton className="h-3 w-full" />
        <Skeleton className="h-3 w-4/5" />
        <Skeleton className="h-3 w-2/3" />
      </div>
    </div>
  );
}
