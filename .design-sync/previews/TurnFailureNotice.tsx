import { TurnFailureNotice } from "tidebreak-desktop-ui";

export function RateLimited() {
  return (
    <div style={{ maxWidth: "42rem" }}>
      <TurnFailureNotice
        category="rate_limited"
        detail="429 rate_limit_error: This request would exceed your account's tokens-per-minute limit of 80,000."
        model={{ id: "claude-opus-4-1", provider: "anthropic" }}
        onRetry={() => {}}
      />
    </div>
  );
}

export function AuthFailure() {
  return (
    <div style={{ maxWidth: "42rem" }}>
      <TurnFailureNotice
        category="auth"
        detail="401 invalid_api_key: Incorrect API key provided: sk-proj-****hV2a."
        model={{ id: "gpt-5.2", provider: "openai" }}
      />
    </div>
  );
}

export function UnknownFailure() {
  return (
    <div style={{ maxWidth: "42rem" }}>
      <TurnFailureNotice
        category="unknown"
        detail="stream ended unexpectedly after 2 chunks (last event id evt_01J9)"
        onRetry={() => {}}
      />
    </div>
  );
}
