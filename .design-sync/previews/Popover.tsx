import {
  Badge,
  Button,
  Popover,
  PopoverContent,
  PopoverTrigger,
  Separator,
} from "tidebreak-desktop-ui";
import { ChevronDown, GitBranch } from "lucide-react";

export function ChatActivity() {
  return (
    <div style={{ display: "flex", justifyContent: "center" }}>
      <Popover open>
        <PopoverTrigger asChild>
          <Button variant="outline" size="sm">
            2 agents running
            <ChevronDown />
          </Button>
        </PopoverTrigger>
        <PopoverContent side="bottom" align="center">
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <div style={{ fontWeight: 600, fontSize: "0.875rem" }}>Activity</div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span style={{ fontFamily: "var(--mono)", fontSize: "0.8125rem" }}>
                tb/fix-retry-test
              </span>
              <div style={{ marginLeft: "auto" }}>
                <Badge variant="success" size="sm">
                  Running
                </Badge>
              </div>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span style={{ fontFamily: "var(--mono)", fontSize: "0.8125rem" }}>
                tb/settings-schema
              </span>
              <div style={{ marginLeft: "auto" }}>
                <Badge variant="warning" size="sm">
                  Waiting
                </Badge>
              </div>
            </div>
            <Separator />
            <div style={{ fontSize: "0.8125rem", color: "var(--muted-foreground)" }}>
              3 files attached · 12 tool calls this turn
            </div>
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}

export function BranchPicker() {
  return (
    <div style={{ display: "flex", justifyContent: "center" }}>
      <Popover open>
        <PopoverTrigger asChild>
          <Button variant="ghost" size="sm">
            <GitBranch />
            main
          </Button>
        </PopoverTrigger>
        <PopoverContent side="bottom" align="center">
          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            <div style={{ fontWeight: 600, fontSize: "0.875rem" }}>Base branch</div>
            <div style={{ fontSize: "0.8125rem", color: "var(--muted-foreground)" }}>
              New workspaces branch from here and open pull requests back into it.
            </div>
            <Separator />
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: 6,
                fontFamily: "var(--mono)",
                fontSize: "0.8125rem",
              }}
            >
              <span>main</span>
              <span>release/1.4</span>
              <span>tb/desktop-arm64</span>
            </div>
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}

export function ApprovalSummary() {
  return (
    <div style={{ display: "flex", justifyContent: "center" }}>
      <Popover open>
        <PopoverTrigger asChild>
          <Button variant="secondary" size="sm">
            Approve edits
          </Button>
        </PopoverTrigger>
        <PopoverContent side="bottom" align="center">
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <div style={{ fontWeight: 600, fontSize: "0.875rem" }}>Edit 3 files</div>
            <div style={{ fontSize: "0.8125rem", color: "var(--muted-foreground)" }}>
              Claude Code wants to write inside the workspace checkout. Nothing leaves the
              branch until you push.
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              <Button size="sm">Allow once</Button>
              <Button size="sm" variant="outline">
                Always allow
              </Button>
            </div>
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}
