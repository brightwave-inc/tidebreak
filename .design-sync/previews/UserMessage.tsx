import { UserMessage } from "tidebreak-desktop-ui";

export function ShortAsk() {
  return (
    <div className="messages-column" style={{ maxWidth: "40rem" }}>
      <UserMessage
        text="Run the suite and fix whatever fails."
        createdAt="2026-08-18T14:31:00Z"
      />
    </div>
  );
}

export function MultiParagraph() {
  return (
    <div className="messages-column" style={{ maxWidth: "40rem" }}>
      <UserMessage
        text={
          "The retry test fails about one run in five on CI.\n\n1. Reproduce it under `--test-threads=1`\n2. Check whether the mock clock races the timer registration\n3. Keep the public API unchanged"
        }
        createdAt="2026-08-18T09:12:00Z"
      />
    </div>
  );
}
