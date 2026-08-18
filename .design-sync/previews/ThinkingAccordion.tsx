import { ThinkingAccordion } from "tidebreak-desktop-ui";

const reasoning = `The failure only reproduces when the mock clock advances before the worker registers its timer, so this is an ordering race, not a tolerance problem.

Two candidate fixes:

1. Register the timer before yielding to the executor — smallest diff, keeps the public API unchanged.
2. Make the mock clock queue advances until a timer exists — broader, touches every test using the clock.

Option 1 localizes the change to \`retry.rs\` and the assertion can move to the *observed* delay. I'll take that.`;

export function Streaming() {
  return (
    <div style={{ maxWidth: "40rem" }}>
      <article className="message message-assistant">
        <ThinkingAccordion text={reasoning} streaming={true} />
      </article>
    </div>
  );
}

export function Settled() {
  return (
    <div style={{ maxWidth: "40rem" }}>
      <article className="message message-assistant">
        <ThinkingAccordion text={reasoning} streaming={false} />
      </article>
    </div>
  );
}
