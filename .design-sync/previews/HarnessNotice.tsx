import { HarnessNotice } from "tidebreak-desktop-ui";

export function Info() {
  return (
    <HarnessNotice
      level="info"
      message="Resumed the engine session from its native checkpoint."
    />
  );
}

export function Warning() {
  return (
    <HarnessNotice
      level="warning"
      message="This engine does not support mid-turn steering; your message will be sent when the current turn ends."
    />
  );
}

export function Error() {
  return (
    <HarnessNotice
      level="error"
      message="The engine exited unexpectedly (exit code 101). Reap the session to start a fresh engine in this workspace."
    />
  );
}
