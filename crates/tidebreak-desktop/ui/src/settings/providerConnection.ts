import { formatDistanceToNowStrict } from "date-fns";

import type {
  ProviderInfo,
  ProviderKind,
  ProviderTestOutcome,
  ProviderTestResult,
} from "../api";
import type { SettingsStatusTone } from "./primitives";

/**
 * Whether a provider needs a saved key before it can run. Ollama and
 * OpenAI-compatible servers usually run on this computer and take none.
 */
export function providerRequiresCredential(kind: ProviderKind): boolean {
  return kind !== "ollama" && kind !== "openai_compatible";
}

/** Whether the reader chooses this provider's address. */
export function providerTakesBaseUrl(kind: ProviderKind): boolean {
  return kind === "ollama" || kind === "openai_compatible";
}

/**
 * Whether the card has what a request needs: it is on, and it has a key or
 * needs none.
 */
export function providerConfigured(info: ProviderInfo): boolean {
  return (
    info.enabled &&
    (info.has_credential || !providerRequiresCredential(info.kind))
  );
}

/**
 * ChatGPT sign-in is checked when it happens, and OpenAI's own rejection
 * marks it for a new sign-in, so it has no connection test.
 */
export function usesChatgptSignIn(info: ProviderInfo): boolean {
  return (
    info.kind === "openai" &&
    info.auth_mode === "chatgpt" &&
    info.has_credential
  );
}

/** Whether a Test button has anything to test on this card. */
export function providerTestable(info: ProviderInfo): boolean {
  if (info.kind === "model_gateway" || usesChatgptSignIn(info)) return false;
  if (providerRequiresCredential(info.kind) && !info.has_credential) {
    return false;
  }
  return info.kind !== "openai_compatible" || Boolean(info.base_url);
}

type BadgeVariant = "success" | "warning" | "critical" | "outline";

const OUTCOME_LABEL: Record<ProviderTestOutcome, string> = {
  connected: "Connected",
  key_rejected: "Key rejected",
  access_denied: "Access denied",
  unreachable: "Unreachable",
  rate_limited: "Rate limited",
  unexpected_answer: "Unexpected answer",
};

const OUTCOME_TONE: Record<ProviderTestOutcome, SettingsStatusTone> = {
  connected: "ready",
  key_rejected: "critical",
  access_denied: "critical",
  unreachable: "critical",
  rate_limited: "warning",
  unexpected_answer: "critical",
};

const TONE_BADGE: Record<SettingsStatusTone, BadgeVariant> = {
  ready: "success",
  neutral: "outline",
  warning: "warning",
  critical: "critical",
};

export function testOutcomeLabel(outcome: ProviderTestOutcome): string {
  return OUTCOME_LABEL[outcome];
}

export function testOutcomeTone(
  outcome: ProviderTestOutcome,
): SettingsStatusTone {
  return OUTCOME_TONE[outcome];
}

/**
 * What a provider card's badge says. A card that is set up says what its
 * last test found, not merely that a key is stored: a key can be mistyped,
 * expired, or out of credit, and an endpoint can be down.
 */
export function providerBadge(
  info: ProviderInfo,
  testing: boolean,
  lastTest: ProviderTestResult | null,
): { label: string; variant: BadgeVariant } {
  if (!providerConfigured(info)) {
    return { label: "Not connected", variant: "outline" };
  }
  if (testing) return { label: "Testing…", variant: "outline" };
  if (usesChatgptSignIn(info)) {
    return { label: "Signed in", variant: "success" };
  }
  if (!lastTest) return { label: "Not tested", variant: "outline" };
  return {
    label: testOutcomeLabel(lastTest.outcome),
    variant: TONE_BADGE[testOutcomeTone(lastTest.outcome)],
  };
}

/** When a test ran, as a person says it: "just now", "5 minutes ago". */
export function testedAgo(testedAt: string, now: Date = new Date()): string {
  const at = new Date(testedAt);
  if (Number.isNaN(at.getTime())) return "at an unknown time";
  if (now.getTime() - at.getTime() < 60_000) return "just now";
  return formatDistanceToNowStrict(at, { addSuffix: true });
}

/**
 * The loopback IP address a clear-text URL names, or `null`. Only an address
 * qualifies for sending a key over HTTP: a name such as `localhost` resolves
 * through files and resolvers Tidebreak does not control.
 */
export function loopbackIpHttpHost(urlText: string): string | null {
  let url: URL;
  try {
    url = new URL(urlText.trim());
  } catch {
    return null;
  }
  if (url.protocol !== "http:" || url.username || url.password) return null;
  const host = url.hostname;
  if (host === "[::1]") return host;
  const parts = host.split(".");
  if (
    parts.length === 4 &&
    parts[0] === "127" &&
    parts.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255)
  ) {
    return host;
  }
  return null;
}
