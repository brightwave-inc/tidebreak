import { useState, type ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  AgentDraft,
  CheckRun,
  CommitControls,
  FileDiff,
  FileRail,
  PullRequestRow,
  RefreshNotice,
  ReviewHeader,
  ReviewThread,
  ViewedControl,
} from "./ReviewComponents";
import { failedLog, reviewComment, reviewFiles, reviewPr } from "./fixtures";
import "./review-proposal.css";

const meta = {
  title: "Code/Review proposal/Components",
  parameters: { layout: "padded" },
} satisfies Meta;
export default meta;
type Story = StoryObj<typeof meta>;
function Surface({ children }: { children: ReactNode }) {
  return (
    <div className="review-proposal max-w-4xl rounded-lg border border-border-subtle bg-background text-foreground">
      {children}
    </div>
  );
}
function HeaderExample({ long = false }: { long?: boolean }) {
  const [note, setNote] = useState("");
  return (
    <Surface>
      <ReviewHeader
        pr={
          long
            ? {
                ...reviewPr,
                title:
                  "Preserve unsent review comments and file selection when multiple repositories finish refreshing at different times",
                head_branch:
                  "thet/preserve-review-context-across-concurrent-repository-refreshes",
              }
            : reviewPr
        }
        onBack={() => setNote("Return to the PR inbox.")}
        onInspect={() => setNote("Open the failed check and its logs.")}
        onRefresh={() => setNote("Sample data refreshed.")}
        onMerge={() => setNote("Open merge confirmation.")}
      />
      {note && (
        <p role="status" className="px-5 pb-3 text-sm text-muted-foreground">
          {note}
        </p>
      )}
    </Surface>
  );
}
export const PullRequestHeader: Story = { render: () => <HeaderExample /> };
export const LongHeader: Story = { render: () => <HeaderExample long /> };
export const InboxRow: Story = {
  render: () => {
    const [opened, setOpened] = useState(false);
    return (
      <Surface>
        <PullRequestRow pr={reviewPr} onOpen={() => setOpened(true)} />
        {opened && (
          <p role="status" className="p-4 text-sm">
            Open the full review workspace.
          </p>
        )}
      </Surface>
    );
  },
};
export const FileDiffWithRail: Story = {
  render: () => {
    const [file, setFile] = useState(reviewFiles[0]!.path);
    const [viewed, setViewed] = useState<string[]>([]);
    return (
      <Surface>
        <div className="review-proposal-files grid grid-cols-[220px_minmax(0,1fr)]">
          <FileRail
            files={reviewFiles}
            selected={file}
            onSelect={setFile}
            viewed={viewed}
          />
          <div className="min-w-0 p-4">
            <FileDiff
              file={reviewFiles.find((item) => item.path === file)!}
              actions={
                <ViewedControl
                  checked={viewed.includes(file)}
                  onChange={(checked) =>
                    setViewed(
                      checked
                        ? [...viewed, file]
                        : viewed.filter((path) => path !== file),
                    )
                  }
                />
              }
            />
          </div>
        </div>
      </Surface>
    );
  },
};
export const InlineReviewThread: Story = {
  render: () => {
    const [context, setContext] = useState<string | null>(null);
    return (
      <Surface>
        <div className="px-4 pb-4">
          <ReviewThread
            path={reviewFiles[0]!.path}
            body={reviewComment.body}
            onAgent={setContext}
          />
        </div>
        {context && (
          <AgentDraft context={context} onClose={() => setContext(null)} />
        )}
      </Surface>
    );
  },
};
export const CheckResults: Story = {
  render: () => {
    const [context, setContext] = useState<string | null>(null);
    return (
      <Surface>
        <div className="px-4">
          <CheckRun
            check={reviewPr.checks[0]!}
            log={failedLog}
            defaultOpen
            onAgent={setContext}
          />
          <CheckRun
            check={{
              name: "desktop / build",
              bucket: "pending",
              detail: "Running · 14 seconds",
            }}
            onAgent={setContext}
          />
          <CheckRun check={reviewPr.checks[1]!} onAgent={setContext} />
        </div>
        {context && (
          <AgentDraft context={context} onClose={() => setContext(null)} />
        )}
      </Surface>
    );
  },
};
export const CommitAndPush: Story = {
  render: () => {
    const [count, setCount] = useState(2);
    const [ahead, setAhead] = useState(2);
    const [note, setNote] = useState("");
    return (
      <Surface>
        <CommitControls
          stagedCount={count}
          ahead={ahead}
          onCommit={(message) => {
            setCount(0);
            setAhead(ahead + 1);
            setNote(`Sample commit: ${message}`);
          }}
          onPush={() => {
            setAhead(0);
            setNote("Sample push complete.");
          }}
        />
        {note && (
          <p role="status" className="px-4 pb-3 text-sm text-muted-foreground">
            {note}
          </p>
        )}
      </Surface>
    );
  },
};
export const AgentMessage: Story = {
  render: () => {
    const [open, setOpen] = useState(true);
    return (
      <Surface>
        {open ? (
          <AgentDraft
            context={`${reviewComment.path}:${reviewComment.line}\n${reviewComment.body}`}
            onClose={() => setOpen(false)}
          />
        ) : (
          <p className="p-4 text-sm">
            Agent draft closed. Reload the story to reopen it.
          </p>
        )}
      </Surface>
    );
  },
};
export const RefreshStates: Story = {
  render: () => {
    const [failed, setFailed] = useState(true);
    return (
      <Surface>
        <div className="space-y-4 p-4">
          <RefreshNotice state="ready" onRetry={() => {}} />
          <RefreshNotice state="refreshing" onRetry={() => {}} />
        </div>
        {failed ? (
          <RefreshNotice state="failed" onRetry={() => setFailed(false)} />
        ) : (
          <p role="status" className="p-4 text-sm">
            Sample refresh succeeded. Your results stay visible.
          </p>
        )}
      </Surface>
    );
  },
};
