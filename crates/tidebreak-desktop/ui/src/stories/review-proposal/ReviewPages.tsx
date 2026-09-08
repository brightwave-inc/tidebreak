import { useState } from "react";
import { ArrowRight, GitBranch, RefreshCw, Search } from "lucide-react";
import { prStatus } from "@/code/prState";
import { STATUS_TEXT } from "@/code/statusTone";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import {
  AgentDraft,
  CheckRun,
  CommitControls,
  FileDiff,
  FileRail,
  PullRequestRow,
  RefreshNotice,
  ReviewEmpty,
  ReviewHeader,
  ReviewLoading,
  ReviewThread,
  ViewedControl,
  type LoadState,
} from "./ReviewComponents";
import {
  failedLog,
  inboxPrs,
  readyPr,
  reviewComment,
  reviewDetail,
  reviewFiles,
  reviewPr,
} from "./fixtures";
import "./review-proposal.css";

export type ReviewScenario =
  | "inbox"
  | "inbox-loading"
  | "inbox-empty"
  | "inbox-failed"
  | "files"
  | "files-loading"
  | "discussion"
  | "checks"
  | "ready"
  | "source"
  | "staged"
  | "clean"
  | "conflict";
type View = "inbox" | "review" | "source";
type Scope = "unstaged" | "staged" | "branch" | "turn";
const tabClass =
  "rounded-none border-b-2 border-transparent px-0 py-3 data-[state=active]:border-foreground data-[state=active]:bg-transparent";

// Index: inbox and file rail. Resource detail: diff, review, and commit form.
export function ReviewPrototype({
  scenario = "files",
}: {
  scenario?: ReviewScenario;
}) {
  const [view, setView] = useState<View>(() =>
    scenario.startsWith("inbox")
      ? "inbox"
      : ["source", "staged", "clean", "conflict"].includes(scenario)
        ? "source"
        : "review",
  );
  const [selectedPr, setSelectedPr] = useState(
    scenario === "ready" ? readyPr : reviewPr,
  );
  const [tab, setTab] = useState(
    scenario === "checks"
      ? "checks"
      : scenario === "discussion"
        ? "discussion"
        : "files",
  );
  const [listState, setListState] = useState<LoadState>(
    scenario === "inbox-loading"
      ? "loading"
      : scenario === "inbox-failed"
        ? "failed"
        : "ready",
  );
  const [filter, setFilter] = useState("attention");
  const [search, setSearch] = useState("");
  const [filesLoading, setFilesLoading] = useState(
    scenario === "files-loading",
  );
  const [scope, setScope] = useState<Scope>(
    scenario === "staged" ? "staged" : "unstaged",
  );
  const [staged, setStaged] = useState<string[]>(
    scenario === "staged" ? [reviewFiles[0]!.path] : [],
  );
  const [committed, setCommitted] = useState<string[]>(
    scenario === "clean" ? reviewFiles.map((file) => file.path) : [],
  );
  const [ahead, setAhead] = useState(scenario === "clean" ? 0 : 2);
  const [selectedFile, setSelectedFile] = useState(reviewFiles[0]!.path);
  const [viewed, setViewed] = useState<string[]>([]);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [savedComments, setSavedComments] = useState<Record<string, string[]>>(
    {},
  );
  const [agentContext, setAgentContext] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const [mergeOpen, setMergeOpen] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [simulateFailure, setSimulateFailure] = useState(
    scenario === "inbox-failed",
  );
  const conflict = scenario === "conflict";
  const dirtyFiles = reviewFiles.filter(
    (file) => !staged.includes(file.path) && !committed.includes(file.path),
  );
  const scopedFiles =
    view !== "source" || scope === "branch" || scope === "turn"
      ? reviewFiles
      : scope === "staged"
        ? reviewFiles.filter((file) => staged.includes(file.path))
        : dirtyFiles;
  const file =
    scopedFiles.find((item) => item.path === selectedFile) ?? scopedFiles[0];
  const draft = drafts[selectedPr.id] ?? "";

  function refresh() {
    setListState("ready");
    setFilesLoading(false);
    setSimulateFailure(false);
    setConfirmed(true);
    setNotice("Sample data refreshed. Your selection and draft stay in place.");
  }
  function openPr(pr = reviewPr) {
    setSelectedPr(pr);
    setView("review");
    setTab("files");
  }
  function startAgent(context: string) {
    setAgentContext(context);
    setNotice("Review context added to the agent draft.");
  }
  function fileArea(source = false) {
    if (filesLoading && !source)
      return <ReviewLoading label="Loading changed files…" />;
    if (!file)
      return (
        <ReviewEmpty
          title={
            scope === "staged" ? "No staged changes" : "No uncommitted changes"
          }
          description={
            scope === "staged"
              ? "Stage a file or hunk to prepare your next commit."
              : "Your working tree is clean. Use Branch to review the changes against main."
          }
        />
      );
    const changeStage = () => {
      if (scope === "staged") {
        setStaged(staged.filter((path) => path !== file.path));
        setScope("unstaged");
        setNotice("Sample file unstaged.");
      } else {
        setStaged([...new Set([...staged, file.path])]);
        setScope("staged");
        setNotice("Sample file staged. Other files stay unstaged.");
      }
    };
    return (
      <div className="review-proposal-files grid grid-cols-[220px_minmax(0,1fr)]">
        <FileRail
          files={scopedFiles}
          selected={file.path}
          onSelect={setSelectedFile}
          viewed={viewed}
        />
        <div className="min-w-0 p-5">
          <FileDiff
            file={file}
            actions={
              source ? (
                <>
                  {(scope === "unstaged" || scope === "staged") &&
                    !conflict && (
                      <>
                        {scope === "unstaged" && (
                          <Button
                            size="sm"
                            variant="outline"
                            onClick={changeStage}
                          >
                            Stage hunk
                          </Button>
                        )}
                        <Button
                          size="sm"
                          variant="outline"
                          onClick={changeStage}
                        >
                          {scope === "staged" ? "Unstage file" : "Stage file"}
                        </Button>
                      </>
                    )}
                </>
              ) : (
                <ViewedControl
                  checked={viewed.includes(file.path)}
                  onChange={(checked) =>
                    setViewed(
                      checked
                        ? [...new Set([...viewed, file.path])]
                        : viewed.filter((path) => path !== file.path),
                    )
                  }
                />
              )
            }
          />
          {!source && (
            <ReviewThread
              key={`${selectedPr.id}:${file.path}`}
              path={file.path}
              body={reviewComment.body}
              onAgent={startAgent}
            />
          )}
          {source && (
            <p className="mt-3 text-xs text-muted-foreground">
              {scope === "staged"
                ? "This is the content your next commit includes."
                : scope === "unstaged"
                  ? "Each sample file has one hunk. Choose only the changes you want to commit."
                  : scope === "branch"
                    ? "Changes on this branch compared with main."
                    : "Changes from the last agent turn."}
            </p>
          )}
        </div>
      </div>
    );
  }
  const listed = (scenario === "inbox-empty" ? [] : inboxPrs).filter(
    (pr) =>
      (filter === "all" || prStatus(pr).group === "attention") &&
      `${pr.title} ${pr.repository.name} ${pr.number}`
        .toLowerCase()
        .includes(search.toLowerCase()),
  );

  return (
    <div
      className="review-proposal flex h-full min-h-0 flex-col bg-background text-foreground"
      data-testid="review-prototype"
      data-scenario={scenario}
    >
      <header className="flex shrink-0 flex-wrap items-center gap-4 border-b border-border-subtle bg-page-background px-4 py-2">
        <span className="flex items-center gap-2 text-sm font-medium">
          <GitBranch className="size-4" />
          Tidebreak
        </span>
        <nav aria-label="Code navigation" className="flex gap-1">
          <Button
            size="sm"
            variant={view === "source" ? "ghost" : "secondary"}
            onClick={() => setView("inbox")}
          >
            Pull requests
          </Button>
          <Button
            size="sm"
            variant={view === "source" ? "secondary" : "ghost"}
            onClick={() => setView("source")}
          >
            Source control
          </Button>
        </nav>
        <span className="ml-auto hidden text-xs text-muted-foreground sm:block">
          Review proposal · sample data
        </span>
      </header>
      <main className="min-h-0 flex-1 overflow-auto">
        {view === "inbox" ? (
          <section className="pb-8">
            <div className="flex flex-wrap items-center justify-between gap-3 px-5 pt-6">
              <div>
                <h1 className="text-xl font-semibold tracking-tight">
                  Pull requests
                </h1>
                <p className="mt-1 text-sm text-muted-foreground">
                  Review what needs your attention.
                </p>
              </div>
              <Button size="sm" variant="outline" onClick={refresh}>
                <RefreshCw />
                Refresh
              </Button>
            </div>
            <div className="flex flex-wrap items-center justify-between gap-3 px-5 py-4">
              <div className="flex gap-1">
                {["attention", "all"].map((value) => (
                  <Button
                    key={value}
                    size="sm"
                    variant={filter === value ? "secondary" : "ghost"}
                    aria-pressed={filter === value}
                    onClick={() => setFilter(value)}
                  >
                    {value === "attention" ? "Needs attention" : "All"}
                  </Button>
                ))}
              </div>
              <label className="relative min-w-0 max-w-72 flex-1">
                <Search className="pointer-events-none absolute top-2 left-2 size-3.5 text-muted-foreground" />
                <Input
                  aria-label="Search pull requests"
                  placeholder="Search pull requests…"
                  size="sm"
                  value={search}
                  onChange={(event) => setSearch(event.target.value)}
                  className="pl-7"
                />
              </label>
            </div>
            {simulateFailure && (
              <RefreshNotice state="failed" onRetry={refresh} />
            )}
            {listState === "loading" ? (
              <ReviewLoading />
            ) : listed.length === 0 ? (
              <ReviewEmpty
                title={search ? "No matching pull requests" : undefined}
                description={
                  search
                    ? "Try a different title, repository, or PR number."
                    : undefined
                }
              />
            ) : (
              listed.map((pr) => (
                <PullRequestRow key={pr.id} pr={pr} onOpen={() => openPr(pr)} />
              ))
            )}
          </section>
        ) : view === "review" ? (
          <>
            <ReviewHeader
              pr={selectedPr}
              onBack={() => setView("inbox")}
              onRefresh={refresh}
              onMerge={() => setMergeOpen(true)}
              onInspect={() =>
                setTab(
                  prStatus(selectedPr).checks.failing ? "checks" : "discussion",
                )
              }
            />
            <Tabs value={tab} onValueChange={setTab}>
              <TabsList
                className="flex w-full justify-start gap-6 rounded-none border-b border-border-subtle px-5"
                aria-label="Pull request sections"
              >
                <TabsTrigger value="files" className={tabClass}>
                  Files <span className="text-xs text-muted-foreground">3</span>
                </TabsTrigger>
                <TabsTrigger value="discussion" className={tabClass}>
                  Discussion
                </TabsTrigger>
                <TabsTrigger value="checks" className={tabClass}>
                  Checks{" "}
                  <span className="text-xs text-muted-foreground">
                    {prStatus(selectedPr).checks.failing
                      ? "1 failed"
                      : "Passed"}
                  </span>
                </TabsTrigger>
              </TabsList>
              <TabsContent value="files" className="mt-0">
                {fileArea()}
              </TabsContent>
              <TabsContent value="discussion" className="m-0 max-w-4xl p-5">
                <h2 className="text-base font-medium">
                  Keep your review in place
                </h2>
                <p className="mt-2 text-md leading-relaxed text-muted-foreground">
                  {reviewDetail.body}
                </p>
                <ReviewThread
                  path={reviewComment.path!}
                  body={reviewComment.body}
                  onAgent={startAgent}
                />
                <form
                  className="mt-6 space-y-2"
                  onSubmit={(event) => {
                    event.preventDefault();
                    if (!draft.trim()) return;
                    setSavedComments({
                      ...savedComments,
                      [selectedPr.id]: [
                        ...(savedComments[selectedPr.id] ?? []),
                        draft.trim(),
                      ],
                    });
                    setDrafts({ ...drafts, [selectedPr.id]: "" });
                    setNotice(
                      "Sample comment added. Nothing was sent to GitHub.",
                    );
                  }}
                >
                  <label
                    htmlFor="review-comment-draft"
                    className="text-sm font-medium"
                  >
                    Comment
                  </label>
                  <Textarea
                    id="review-comment-draft"
                    rows={3}
                    placeholder="Add a comment to the review…"
                    value={draft}
                    onChange={(event) =>
                      setDrafts({
                        ...drafts,
                        [selectedPr.id]: event.target.value,
                      })
                    }
                  />
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <p className="text-xs text-muted-foreground">
                      Your draft stays with this PR when you switch views.
                    </p>
                    <Button size="sm" type="submit" disabled={!draft.trim()}>
                      Add sample comment
                    </Button>
                  </div>
                </form>
                {savedComments[selectedPr.id]?.map((comment, i) => (
                  <p
                    key={i}
                    className="mt-4 border-t border-border-subtle pt-3 text-sm"
                  >
                    <span className="font-medium">You · </span>
                    {comment}
                  </p>
                ))}
              </TabsContent>
              <TabsContent value="checks" className="m-0 p-5">
                <h2 className="mb-2 text-base font-medium">
                  {prStatus(selectedPr).checks.failing
                    ? "One required check failed"
                    : "All checks passed"}
                </h2>
                {[...selectedPr.checks]
                  .sort(
                    (a, b) =>
                      Number(b.bucket === "fail") - Number(a.bucket === "fail"),
                  )
                  .map((check) => (
                    <CheckRun
                      key={check.name}
                      check={check}
                      log={check.bucket === "fail" ? failedLog : undefined}
                      defaultOpen={check.bucket === "fail"}
                      onAgent={startAgent}
                    />
                  ))}
              </TabsContent>
            </Tabs>
          </>
        ) : (
          <>
            <header className="px-5 pt-5">
              <p className="text-xs text-muted-foreground">
                Workspace · tidebreak
              </p>
              <div className="mt-2 flex flex-wrap items-center justify-between gap-3">
                <div>
                  <h1 className="text-xl font-semibold tracking-tight">
                    Source control
                  </h1>
                  <p className="mt-2 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                    <GitBranch className="size-3" />
                    <span className="font-mono">thet/review-drafts</span>
                    <ArrowRight className="size-3" />
                    <span className="font-mono">main</span>
                    <span>
                      ·{" "}
                      {ahead
                        ? `${ahead} commits to push`
                        : "Up to date with origin"}
                    </span>
                  </p>
                </div>
                <Button size="sm" variant="outline" onClick={() => openPr()}>
                  View PR #{reviewPr.number}
                </Button>
              </div>
            </header>
            {conflict && (
              <div
                role="alert"
                className="notice-surface notice-critical mx-5 mt-4 p-3"
              >
                <p className="text-sm font-medium">
                  Resolve one conflict before committing
                </p>
                <p className="mt-1 font-mono text-xs wrap-anywhere">
                  {reviewFiles[0]!.path}
                </p>
                <Button
                  className="mt-2"
                  size="sm"
                  variant="outline"
                  onClick={() =>
                    startAgent(
                      `Resolve the conflict in ${reviewFiles[0]!.path}. Keep unrelated changes.`,
                    )
                  }
                >
                  Ask agent to resolve
                </Button>
              </div>
            )}
            <Tabs
              value={scope}
              onValueChange={(value) => setScope(value as Scope)}
              className="mt-4"
            >
              <TabsList
                className="flex w-full justify-start gap-5 rounded-none border-b border-border-subtle px-5"
                aria-label="Diff scope"
              >
                {(
                  [
                    ["unstaged", `Unstaged ${dirtyFiles.length}`],
                    ["staged", `Staged ${staged.length}`],
                    ["branch", "Branch"],
                    ["turn", "Last turn"],
                  ] as const
                ).map(([value, label]) => (
                  <TabsTrigger key={value} className={tabClass} value={value}>
                    {label}
                  </TabsTrigger>
                ))}
              </TabsList>
              {(["unstaged", "staged", "branch", "turn"] as const).map(
                (value) => (
                  <TabsContent className="mt-0" key={value} value={value}>
                    {fileArea(true)}
                  </TabsContent>
                ),
              )}
            </Tabs>
            <CommitControls
              stagedCount={staged.length}
              ahead={ahead}
              blocked={conflict}
              onCommit={(message) => {
                setCommitted([...committed, ...staged]);
                setStaged([]);
                setScope("unstaged");
                setAhead(ahead + 1);
                setNotice(`Sample commit created: ${message}`);
              }}
              onPush={() => {
                setAhead(0);
                setNotice("Sample push complete. Nothing was sent to GitHub.");
              }}
            />
          </>
        )}
        {agentContext !== null && (
          <AgentDraft
            key={agentContext}
            context={agentContext}
            onClose={() => setAgentContext(null)}
          />
        )}
      </main>
      <footer className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-t border-border-subtle px-4 py-2 text-xs text-muted-foreground">
        <span>
          {confirmed
            ? "Last confirmed just now"
            : "Last confirmed 20 seconds ago"}
        </span>
        <span role="status" className="text-foreground" aria-live="polite">
          {notice || "Storybook only · no GitHub or Git writes"}
        </span>
      </footer>
      <Dialog open={mergeOpen} onOpenChange={setMergeOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Merge PR #{selectedPr.number}?</DialogTitle>
            <DialogDescription>
              Squash {selectedPr.head_branch} into {selectedPr.base_branch}.
              This Storybook action does not contact GitHub.
            </DialogDescription>
          </DialogHeader>
          <div className="rounded-lg border border-border-subtle p-3 font-mono text-sm">
            {selectedPr.repository.owner}/{selectedPr.repository.name}
            <p className="mt-1 text-xs text-muted-foreground">
              Displayed head: {selectedPr.head_sha}
            </p>
          </div>
          <p className={cn("text-sm", STATUS_TEXT.ready)}>
            Checks passed. Review approved.
          </p>
          <DialogFooter>
            <Button
              size="sm"
              variant="outline"
              onClick={() => setMergeOpen(false)}
            >
              Cancel
            </Button>
            <Button
              size="sm"
              onClick={() => {
                setMergeOpen(false);
                setNotice(
                  "Sample merge confirmed. Nothing was merged on GitHub.",
                );
              }}
            >
              Confirm sample merge
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
