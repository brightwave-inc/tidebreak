import { useState } from "react";
import { SegmentedControl } from "tidebreak-desktop-ui";

export function ModeSwitch() {
  const [mode, setMode] = useState<"chat" | "code">("code");
  return (
    <div style={{ width: 240 }}>
      <SegmentedControl
        aria-label="Mode"
        value={mode}
        onValueChange={setMode}
        options={[
          { value: "chat", label: "Chat" },
          { value: "code", label: "Code" },
        ]}
      />
    </div>
  );
}

export function ThreeWay() {
  const [sort, setSort] = useState<"repo" | "status" | "created">("status");
  return (
    <div style={{ width: 320 }}>
      <SegmentedControl
        aria-label="Sort workspaces"
        value={sort}
        onValueChange={setSort}
        options={[
          { value: "repo", label: "By repo" },
          { value: "status", label: "By status" },
          { value: "created", label: "Created" },
        ]}
      />
    </div>
  );
}
