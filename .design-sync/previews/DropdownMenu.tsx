import {
  Button,
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "tidebreak-desktop-ui";
import {
  Archive,
  ChevronDown,
  Copy,
  ExternalLink,
  GitPullRequest,
  Pencil,
  RefreshCw,
  Terminal,
  Trash2,
} from "lucide-react";

export function WorkspaceMenu() {
  return (
    <div style={{ display: "flex", justifyContent: "center" }}>
      <DropdownMenu defaultOpen modal={false}>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" size="sm">
            Fix flaky retry test
            <ChevronDown />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="center" style={{ minWidth: "15rem" }}>
          <DropdownMenuGroup>
            <DropdownMenuItem>
              <GitPullRequest />
              Open pull request
            </DropdownMenuItem>
            <DropdownMenuItem>
              <Terminal />
              Open terminal
            </DropdownMenuItem>
            <DropdownMenuItem>
              <Copy />
              Copy branch name
            </DropdownMenuItem>
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuGroup>
            <DropdownMenuItem>
              <Pencil />
              Rename workspace
            </DropdownMenuItem>
            <DropdownMenuItem>
              <Archive />
              Archive
            </DropdownMenuItem>
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive">
            <Trash2 />
            Delete workspace
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

export function PermissionModes() {
  return (
    <div style={{ display: "flex", justifyContent: "center" }}>
      <DropdownMenu defaultOpen modal={false}>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="sm">
            Ask
            <ChevronDown />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="center" style={{ minWidth: "14rem" }}>
          <DropdownMenuCheckboxItem checked={false}>Plan</DropdownMenuCheckboxItem>
          <DropdownMenuCheckboxItem checked>Ask</DropdownMenuCheckboxItem>
          <DropdownMenuCheckboxItem checked={false}>Auto</DropdownMenuCheckboxItem>
          <DropdownMenuCheckboxItem checked={false} disabled>
            Allow all
          </DropdownMenuCheckboxItem>
          <DropdownMenuSeparator />
          <DropdownMenuCheckboxItem checked variant="switch">
            Keep the terminal attached
          </DropdownMenuCheckboxItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

export function ProjectMenuWithSubmenu() {
  return (
    <div style={{ display: "flex", justifyContent: "flex-start" }}>
      <DropdownMenu defaultOpen modal={false}>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" size="sm">
            tidebreak
            <ChevronDown />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" style={{ minWidth: "13rem" }}>
          <DropdownMenuItem>
            <RefreshCw />
            Fetch and prune
          </DropdownMenuItem>
          <DropdownMenuItem>
            <ExternalLink />
            Reveal in Finder
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuSub open>
            <DropdownMenuSubTrigger>New workspace from…</DropdownMenuSubTrigger>
            <DropdownMenuSubContent>
              <DropdownMenuItem>main</DropdownMenuItem>
              <DropdownMenuItem>release/1.4</DropdownMenuItem>
              <DropdownMenuItem>Current checkout</DropdownMenuItem>
            </DropdownMenuSubContent>
          </DropdownMenuSub>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
