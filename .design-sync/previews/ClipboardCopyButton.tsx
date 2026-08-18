import { ClipboardCopyButton } from "tidebreak-desktop-ui";

// In the app this button sits in a message footer and only its hover
// background changes; standalone cells keep it visible at its real size.

export function MessageCopy() {
  return (
    <div className="message-footer" style={{ maxWidth: "24rem" }}>
      <ClipboardCopyButton
        value="The race is fixed: the timer now registers before the executor yields."
        label="Copy"
        copiedAnnouncement="Message copied to clipboard."
        failedAnnouncement="Message could not be copied."
        className="message-copy"
      />
      <span className="message-footer-spacer" />
    </div>
  );
}

export function BesideCommand() {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: "0.5rem",
        maxWidth: "28rem",
      }}
    >
      <code
        style={{
          fontSize: "0.78rem",
          padding: "0.3rem 0.55rem",
          borderRadius: 6,
          background: "var(--muted)",
        }}
      >
        cargo check --workspace --locked
      </code>
      <ClipboardCopyButton
        value="cargo check --workspace --locked"
        label="Copy command"
        copiedAnnouncement="Command copied to clipboard."
        failedAnnouncement="Command could not be copied."
        className="message-copy"
      />
    </div>
  );
}
