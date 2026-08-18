import { DomainFavicon } from "tidebreak-desktop-ui";

const SITES = [
  "https://github.com/tidebreak/tidebreak/pull/2182",
  "https://docs.rs/tokio/latest/tokio/",
  "https://crates.io/crates/serde",
];

export function Sites() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      {SITES.map((url) => (
        <div key={url} style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <DomainFavicon url={url} />
          <code style={{ fontSize: 11 }}>{url}</code>
        </div>
      ))}
      <p className="text-muted-foreground" style={{ fontSize: 11, margin: 0 }}>
        Always the local globe — the host is never disclosed to a favicon
        service.
      </p>
    </div>
  );
}

export function SizesAndInline() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 16 }}>
        {(["size-3", "size-4", "size-5", "size-6"] as const).map((size) => (
          <div
            key={size}
            style={{
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              gap: 4,
            }}
          >
            <DomainFavicon url="https://docs.rs/tokio" className={size} />
            <code style={{ fontSize: 10 }}>{size}</code>
          </div>
        ))}
      </div>
      <div className="bg-background flex max-w-md items-center gap-2 rounded-md border px-3 py-2">
        <DomainFavicon url="https://github.com/tidebreak/tidebreak/pull/2182" />
        <span className="truncate text-sm">
          Ship ARM64 packages and cross-platform updates
        </span>
        <span className="text-muted-foreground ml-auto shrink-0 text-xs">
          github.com
        </span>
      </div>
    </div>
  );
}
