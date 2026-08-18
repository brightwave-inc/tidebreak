import { Button } from "tidebreak-desktop-ui";

export function Variants() {
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
      <Button>New workspace</Button>
      <Button variant="secondary">Push branch</Button>
      <Button variant="outline">Open pull request</Button>
      <Button variant="ghost">Archive</Button>
      <Button variant="destructive">Delete repo</Button>
      <Button variant="link">View checks</Button>
    </div>
  );
}

export function Sizes() {
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
      <Button size="lg">Start session</Button>
      <Button>Start session</Button>
      <Button size="sm">Start session</Button>
      <Button size="xs">Start session</Button>
    </div>
  );
}

export function Disabled() {
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
      <Button disabled>Merging…</Button>
      <Button variant="outline" disabled>
        Enable auto-merge
      </Button>
    </div>
  );
}
