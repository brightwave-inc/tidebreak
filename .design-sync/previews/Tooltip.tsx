import {
  Button,
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "tidebreak-desktop-ui";
import { GitPullRequest, PanelLeft } from "lucide-react";

export function ContextUsage() {
  return (
    <TooltipProvider delayDuration={0}>
      <div style={{ display: "flex", justifyContent: "center" }}>
        <Tooltip open>
          <TooltipTrigger asChild>
            <Button variant="outline" size="sm">
              72% context
            </Button>
          </TooltipTrigger>
          <TooltipContent side="bottom">
            144k of 200k tokens used. Tidebreak compacts the transcript at 90%.
          </TooltipContent>
        </Tooltip>
      </div>
    </TooltipProvider>
  );
}

export function IconButtonTip() {
  return (
    <TooltipProvider delayDuration={0}>
      <div style={{ display: "flex", justifyContent: "center" }}>
        <Tooltip open>
          <TooltipTrigger asChild>
            <Button variant="ghost" size="icon" aria-label="Toggle sidebar">
              <PanelLeft />
            </Button>
          </TooltipTrigger>
          <TooltipContent side="bottom">Toggle sidebar</TooltipContent>
        </Tooltip>
      </div>
    </TooltipProvider>
  );
}

export function RichTip() {
  return (
    <TooltipProvider delayDuration={0}>
      <div style={{ display: "flex", justifyContent: "center" }}>
        <Tooltip open>
          <TooltipTrigger asChild>
            <Button variant="secondary" size="sm">
              <GitPullRequest />
              PR #2183
            </Button>
          </TooltipTrigger>
          <TooltipContent side="bottom">
            <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
              <span>Retry newly created draft lookup</span>
              <span style={{ fontFamily: "var(--mono)" }}>tb/fix-retry-test → main</span>
              <span>3 checks passing · +128 −41</span>
            </div>
          </TooltipContent>
        </Tooltip>
      </div>
    </TooltipProvider>
  );
}
