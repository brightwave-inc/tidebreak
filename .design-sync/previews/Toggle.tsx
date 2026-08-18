import { Toggle } from "tidebreak-desktop-ui";
import { Eye, Regex, WrapText } from "lucide-react";

export function Variants() {
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
      <Toggle defaultPressed aria-label="Wrap lines">
        <WrapText />
        Wrap lines
      </Toggle>
      <Toggle aria-label="Show whitespace">
        <Eye />
        Whitespace
      </Toggle>
      <Toggle variant="outline" defaultPressed aria-label="Regex search">
        <Regex />
        Regex
      </Toggle>
      <Toggle variant="outline" aria-label="Match case">
        Match case
      </Toggle>
    </div>
  );
}

export function Sizes() {
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
      <Toggle variant="outline" size="lg" defaultPressed>
        <WrapText />
        Wrap lines
      </Toggle>
      <Toggle variant="outline" defaultPressed>
        <WrapText />
        Wrap lines
      </Toggle>
      <Toggle variant="outline" size="sm" defaultPressed>
        <WrapText />
        Wrap lines
      </Toggle>
    </div>
  );
}

export function Disabled() {
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
      <Toggle disabled>
        <Regex />
        Regex
      </Toggle>
      <Toggle variant="outline" disabled defaultPressed>
        <WrapText />
        Wrap lines
      </Toggle>
    </div>
  );
}
