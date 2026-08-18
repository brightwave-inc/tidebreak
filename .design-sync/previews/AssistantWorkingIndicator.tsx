import { AssistantWorkingIndicator } from "tidebreak-desktop-ui";

export function Working() {
  return (
    <div style={{ maxWidth: "40rem" }}>
      <AssistantWorkingIndicator />
    </div>
  );
}

export function Compacting() {
  return (
    <div style={{ maxWidth: "40rem" }}>
      <AssistantWorkingIndicator compacting />
    </div>
  );
}
