import { Logomark } from "tidebreak-desktop-ui";

export function Sizes() {
  return (
    <div style={{ display: "flex", alignItems: "flex-end", gap: 24 }}>
      {[
        [24, 13],
        [48, 26],
        [96, 52],
      ].map(([width, height]) => (
        <div
          key={width}
          style={{
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            gap: 6,
          }}
        >
          <Logomark width={width} height={height} />
          <code style={{ fontSize: 11 }}>{width}px</code>
        </div>
      ))}
    </div>
  );
}

export function InkColors() {
  return (
    <div style={{ display: "flex", alignItems: "flex-end", gap: 24 }}>
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 6,
        }}
      >
        <Logomark width={48} height={26} />
        <code style={{ fontSize: 11 }}>foreground</code>
      </div>
      <div
        className="text-muted-foreground"
        style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 6,
        }}
      >
        <Logomark width={48} height={26} />
        <code style={{ fontSize: 11 }}>muted</code>
      </div>
      <div
        className="bg-foreground text-background rounded-md"
        style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 6,
          padding: "12px 16px",
        }}
      >
        <Logomark width={48} height={26} />
        <code style={{ fontSize: 11 }}>inverted surface</code>
      </div>
    </div>
  );
}

export function InTitleBar() {
  return (
    <div
      className="bg-background flex items-center gap-2 rounded-lg border px-3 py-2"
      style={{ width: 320 }}
    >
      <Logomark width={20} height={11} />
      <span className="text-sm font-medium">Tidebreak</span>
      <span className="text-muted-foreground ml-auto text-xs">
        tb/fix-retry-test
      </span>
    </div>
  );
}
