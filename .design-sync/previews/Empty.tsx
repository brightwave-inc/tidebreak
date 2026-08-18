import {
  Button,
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "tidebreak-desktop-ui";
import { FolderGit2, SearchX } from "lucide-react";

export function NoWorkspaces() {
  return (
    <div style={{ maxWidth: "32rem", border: "1px dashed var(--border)", borderRadius: 8 }}>
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <FolderGit2 />
          </EmptyMedia>
          <EmptyTitle>No workspaces yet</EmptyTitle>
          <EmptyDescription>
            Create a workspace to hand a branch to an agent. Each workspace gets its own
            checkout, terminal, and transcript.
          </EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <Button>New workspace</Button>
        </EmptyContent>
      </Empty>
    </div>
  );
}

export function NoSearchResults() {
  return (
    <div style={{ maxWidth: "32rem", border: "1px dashed var(--border)", borderRadius: 8 }}>
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <SearchX />
          </EmptyMedia>
          <EmptyTitle>No sessions match</EmptyTitle>
          <EmptyDescription>
            Nothing in this project matches “retry backoff”. Clear the filter or search
            across all projects.
          </EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <Button variant="outline" size="sm">
            Clear filter
          </Button>
        </EmptyContent>
      </Empty>
    </div>
  );
}
