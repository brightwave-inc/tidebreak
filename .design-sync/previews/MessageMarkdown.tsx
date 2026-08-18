import { MessageMarkdown } from "tidebreak-desktop-ui";

const reply = `## Fixing the retry test

The failure is a race in \`test_retry_backoff\`: the mock clock advances before
the worker registers its timer. Two changes fix it:

1. Register the timer **before** yielding to the executor.
2. Assert on the *observed* delay, not the scheduled one.

\`\`\`rust
let timer = clock.register(Duration::from_millis(250));
executor.yield_now().await;
assert_eq!(timer.observed_delay(), Duration::from_millis(250));
\`\`\`

| Run | Before | After |
| --- | --- | --- |
| 500 iterations | 12 failures | 0 failures |

The fix keeps the public API unchanged.`;

export function AssistantReply() {
  return (
    <div style={{ maxWidth: "42rem" }}>
      <MessageMarkdown>{reply}</MessageMarkdown>
    </div>
  );
}

export function InlineElements() {
  return (
    <div style={{ maxWidth: "42rem" }}>
      <MessageMarkdown>
        {"Run `cargo check --workspace --locked` before pushing — the CI lane fails on lockfile drift. See the [release notes](https://example.com/releases) for details."}
      </MessageMarkdown>
    </div>
  );
}
