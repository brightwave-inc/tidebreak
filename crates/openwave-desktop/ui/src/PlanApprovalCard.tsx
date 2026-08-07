import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactElement,
  type ReactNode,
} from "react";
import {
  Compass,
  CornerDownLeft,
  Maximize2,
  MessageSquare,
  MessageSquarePlus,
  Minimize2,
  Trash2,
  X,
} from "lucide-react";
import type { PendingPlanApproval, PlanDecision } from "./api";
import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { MessageMarkdown } from "./MessageMarkdown";
import { serializePlanComments, usePlanComments } from "./PlanComments";

/**
 * One block of the plan, with the affordance for commenting on it.
 *
 * The comment is addressed to the block's own source text, so it survives the
 * plan being re-rendered and reads back as a quote of exactly what the reader
 * was looking at when they wrote it.
 */
function CommentBlock({
  callId,
  blockText,
  disabled,
  children,
}: {
  callId: string;
  blockText: string;
  disabled: boolean;
  children: ReactElement;
}) {
  const [open, setOpen] = useState(false);
  const [hovered, setHovered] = useState(false);
  const [draft, setDraft] = useState("");
  const setComment = usePlanComments((state) => state.setComment);
  const removeComment = usePlanComments((state) => state.removeComment);
  const existing = usePlanComments(
    (state) =>
      (state.byCall[callId] ?? []).find(
        (entry) => entry.blockText === blockText,
      )?.comment ?? null,
  );
  const hasComment = existing !== null;

  useEffect(() => {
    if (!open) setHovered(false);
  }, [open]);

  const save = () => {
    const trimmed = draft.trim();
    if (!trimmed) return;
    setComment(callId, blockText, trimmed);
    setOpen(false);
  };

  return (
    <div
      className="flex items-start gap-1"
      onMouseOver={(event) => {
        event.stopPropagation();
        setHovered(true);
      }}
      onMouseOut={() => setHovered(false)}
    >
      <div
        className={cn(
          "min-w-0 flex-1 rounded px-1 transition-colors",
          hasComment && "bg-accent",
          !hasComment && (open || hovered) && "bg-accent/50",
        )}
      >
        {children}
      </div>
      <div className="mt-0.5 shrink-0">
        <Popover
          open={open}
          onOpenChange={(next) => {
            if (next) setDraft(existing ?? "");
            setOpen(next);
          }}
        >
          <PopoverTrigger asChild>
            <button
              type="button"
              disabled={disabled}
              className={cn(
                "hover:text-foreground hover:bg-muted rounded p-2 transition-opacity",
                hasComment
                  ? "text-primary opacity-100"
                  : cn(
                      "text-muted-foreground",
                      hovered ? "opacity-100" : "opacity-0",
                    ),
              )}
              aria-label={hasComment ? "Edit comment" : "Add comment"}
              title={hasComment ? "Edit comment" : "Add comment for this block"}
            >
              {hasComment ? (
                <MessageSquare aria-hidden="true" className="size-4" />
              ) : (
                <MessageSquarePlus aria-hidden="true" className="size-4" />
              )}
            </button>
          </PopoverTrigger>
          <PopoverContent side="right" align="start" className="w-72">
            <div className="space-y-3">
              <Textarea
                maxLength={2000}
                placeholder="What should change?"
                aria-label="What should change"
                value={draft}
                onChange={(event) => setDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && (event.metaKey || event.ctrlKey))
                    save();
                }}
                className="min-h-20"
                autoFocus
              />
              <div className="flex items-center justify-between gap-2">
                {hasComment ? (
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Delete comment"
                    onClick={() => {
                      removeComment(callId, blockText);
                      setDraft("");
                      setOpen(false);
                    }}
                  >
                    <Trash2 aria-hidden="true" />
                  </Button>
                ) : (
                  <div />
                )}
                <div className="flex gap-2">
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => setOpen(false)}
                  >
                    Cancel
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    onClick={save}
                    disabled={!draft.trim()}
                  >
                    Save
                  </Button>
                </div>
              </div>
            </div>
          </PopoverContent>
        </Popover>
      </div>
    </div>
  );
}

/**
 * The decision card for a plan the agent proposed from plan mode.
 *
 * Approving is the mode hand-off: the server moves the chat out of plan mode
 * and the resumed turn executes with its full tool surface, so the primary
 * action says exactly that. Revising is done in the plan itself — a comment on
 * any block collects into the feedback the rejected call carries back, which
 * keeps the chat in plan mode for another pass.
 */
export function PlanApprovalCard({
  request,
  working,
  error,
  onDecide,
  onCancel,
  onFullscreenChange,
}: {
  request: PendingPlanApproval;
  working: boolean;
  error: string | undefined;
  onDecide: (decision: PlanDecision) => void;
  onCancel: () => void;
  onFullscreenChange?: (fullscreen: boolean) => void;
}) {
  const callId = request.callId;
  const [fullscreen, setFullscreen] = useState(false);
  const [showTopFade, setShowTopFade] = useState(false);
  const [showBottomFade, setShowBottomFade] = useState(false);
  const scroller = useRef<HTMLDivElement>(null);

  const hydrate = usePlanComments((state) => state.hydrate);
  const clear = usePlanComments((state) => state.clear);
  const comments = usePlanComments((state) => state.byCall[callId]);
  const commentCount = comments?.length ?? 0;

  useEffect(() => hydrate(callId), [callId, hydrate]);

  const checkScroll = useCallback(() => {
    const node = scroller.current;
    if (!node) return;
    const scrollable = node.scrollHeight > node.clientHeight;
    setShowTopFade(scrollable && node.scrollTop > 0);
    setShowBottomFade(
      scrollable && node.scrollTop + node.clientHeight < node.scrollHeight - 1,
    );
  }, []);

  useEffect(() => checkScroll(), [checkScroll, request.plan]);

  const wrapBlock = useCallback(
    (source: string, element: ReactElement): ReactNode => (
      <CommentBlock callId={callId} blockText={source} disabled={working}>
        {element}
      </CommentBlock>
    ),
    [callId, working],
  );

  const accept = () => {
    clear(callId);
    onDecide({ decision: "accept" });
  };

  const requestChanges = () => {
    const feedback = serializePlanComments(comments ?? []);
    clear(callId);
    onDecide(
      feedback ? { decision: "reject", feedback } : { decision: "reject" },
    );
  };

  const toggleFullscreen = () => {
    setFullscreen((previous) => {
      const next = !previous;
      onFullscreenChange?.(next);
      return next;
    });
  };

  const header = (
    <>
      <div className="py-1">
        <Compass
          aria-hidden="true"
          className="text-muted-foreground size-4 shrink-0"
        />
      </div>
      <h3
        id={`plan-${callId}`}
        className={cn(
          "min-w-0 font-medium break-words",
          fullscreen && "text-sm",
        )}
      >
        {request.title}
      </h3>
      <div className="flex-1" />
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        className="text-muted-foreground shrink-0"
        disabled={working}
        onClick={toggleFullscreen}
        aria-label={fullscreen ? "Exit full screen" : "Full screen"}
        title={fullscreen ? "Exit full screen" : "Full screen"}
      >
        {fullscreen ? (
          <Minimize2 aria-hidden="true" />
        ) : (
          <Maximize2 aria-hidden="true" />
        )}
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        className="text-muted-foreground shrink-0"
        disabled={working}
        onClick={onCancel}
        aria-label="Cancel turn"
        title="Cancel turn"
      >
        <X aria-hidden="true" />
      </Button>
    </>
  );

  const footer =
    commentCount > 0 ? (
      <>
        <span className="text-muted-foreground grow text-sm">
          {commentCount} edit{commentCount > 1 ? "s" : ""} added
        </span>
        <Button
          type="button"
          variant="outline"
          disabled={working}
          onClick={() => clear(callId)}
        >
          Cancel edits
        </Button>
        <Button type="button" disabled={working} onClick={requestChanges}>
          {working ? "Sending…" : "Update plan"}
          {!working && <CornerDownLeft aria-hidden="true" />}
        </Button>
      </>
    ) : (
      <>
        <span className="text-muted-foreground grow text-sm">
          Hover over plan to edit
        </span>
        <Button type="button" disabled={working} onClick={accept}>
          {working ? "Sending…" : "Execute plan"}
          {!working && <CornerDownLeft aria-hidden="true" />}
        </Button>
      </>
    );

  const errorNotice = error ? (
    <p className="text-destructive text-xs break-words" role="alert">
      {error}
    </p>
  ) : null;

  const plan = (
    <MessageMarkdown wrapBlock={wrapBlock}>{request.plan}</MessageMarkdown>
  );

  if (fullscreen) {
    return (
      <section
        className="flex h-full flex-col"
        aria-labelledby={`plan-${callId}`}
        aria-busy={working}
      >
        <div className="flex items-start gap-2 border-b px-4 py-3">
          {header}
        </div>
        <div className="flex-1 overflow-y-auto p-4 text-sm">{plan}</div>
        <div className="border-t px-4 py-3">
          {errorNotice}
          <div className="flex items-center justify-end gap-3">{footer}</div>
        </div>
      </section>
    );
  }

  return (
    <section
      className="bg-background rounded-lg border p-4"
      aria-labelledby={`plan-${callId}`}
      aria-busy={working}
    >
      <div className="mb-2 flex items-start gap-2">{header}</div>

      <div className="relative">
        {showTopFade && (
          <div className="from-background pointer-events-none absolute top-0 right-0 left-0 z-10 h-16 bg-gradient-to-b to-transparent" />
        )}
        <div
          ref={scroller}
          onScroll={checkScroll}
          className="max-h-[400px] max-w-none overflow-y-auto text-sm"
        >
          {plan}
        </div>
        {showBottomFade && (
          <div className="from-background pointer-events-none absolute right-0 bottom-0 left-0 z-10 h-16 bg-gradient-to-t to-transparent" />
        )}
      </div>

      {errorNotice}

      <div className="mt-6 flex items-center justify-end gap-3">{footer}</div>
    </section>
  );
}
