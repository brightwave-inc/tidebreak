import { MessageFooter, MessageMarkdown } from "tidebreak-desktop-ui";

// In the app the timestamp fades in on message hover; a static capture never
// hovers, so reveal it here.
const showTimestamp = `.ds-preview-cell .message-footer time { opacity: 1; }`;

function minutesAgo(minutes: number): string {
  return new Date(Date.now() - minutes * 60_000).toISOString();
}

function daysAgo(days: number): string {
  const d = new Date(Date.now() - days * 86_400_000);
  d.setHours(9, 12, 0, 0);
  return d.toISOString();
}

export function AssistantTurnEnd() {
  return (
    <div className="ds-preview-cell" style={{ maxWidth: "40rem" }}>
      <style>{showTimestamp}</style>
      <article className="message message-assistant">
        <MessageMarkdown>
          {"The race is fixed: the timer now registers before the executor yields, so `test_retry_backoff` passes across 500 iterations."}
        </MessageMarkdown>
        <MessageFooter
          role="assistant"
          text="The race is fixed: the timer now registers before the executor yields."
          createdAt={minutesAgo(4)}
          settled
        />
      </article>
    </div>
  );
}

export function UserMessage() {
  return (
    <div className="ds-preview-cell" style={{ maxWidth: "40rem" }}>
      <style>{showTimestamp}</style>
      <div className="message-user-frame">
        <article className="message message-user">
          <MessageMarkdown>
            {"Can you fix the flaky retry test in the scheduler crate?"}
          </MessageMarkdown>
        </article>
        <MessageFooter
          role="user"
          text="Can you fix the flaky retry test in the scheduler crate?"
          createdAt={minutesAgo(9)}
        />
      </div>
    </div>
  );
}

export function EarlierDay() {
  return (
    <div className="ds-preview-cell" style={{ maxWidth: "40rem" }}>
      <style>{showTimestamp}</style>
      <article className="message message-assistant">
        <MessageMarkdown>
          {"Opened PR #2151 with the lockfile pin; CI lanes are green."}
        </MessageMarkdown>
        <MessageFooter
          role="assistant"
          text="Opened PR #2151 with the lockfile pin; CI lanes are green."
          createdAt={daysAgo(3)}
          settled
        />
      </article>
    </div>
  );
}
