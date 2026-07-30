import { create } from "zustand";

import type {
  ModelSelectionKey,
  NetworkPolicy,
  PermissionMode,
  ReasoningEffort,
} from "./api";

const MODEL_KEY = "openwave.new-chat-model";
const REASONING_EFFORT_KEY = "openwave.new-chat-reasoning-effort";
const PERMISSION_MODE_KEY = "openwave.new-chat-permission-mode";
const NETWORK_POLICY_KEY = "openwave.new-chat-network-policy";

const REASONING_EFFORTS: readonly ReasoningEffort[] = [
  "none",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];

const PERMISSION_MODES: readonly PermissionMode[] = [
  "plan",
  "ask",
  "auto",
  "allow",
];


function read(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string | null): void {
  try {
    if (value === null) window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, value);
  } catch {
    // Persisting a pre-chat choice is best-effort; the session still holds it.
  }
}

/**
 * A stored value is only honored when this build still recognizes it. A model
 * key is checked against the live catalog at the point of use rather than
 * here — the catalog is not loaded yet when this store initializes, and a
 * selection whose provider was since removed should read as "unavailable"
 * exactly the way it does inside a chat.
 */
function readEnum<T extends string>(
  key: string,
  allowed: readonly T[],
): T | null {
  const stored = read(key);
  return allowed.find((value) => value === stored) ?? null;
}

type NewChatSettings = {
  model: ModelSelectionKey | null;
  reasoningEffort: ReasoningEffort | null;
  permissionMode: PermissionMode | null;
  networkPolicy: NetworkPolicy;
  setModel: (model: ModelSelectionKey | null) => void;
  setReasoningEffort: (effort: ReasoningEffort | null) => void;
  setPermissionMode: (mode: PermissionMode) => void;
  setNetworkPolicy: (policy: NetworkPolicy) => void;
};

function readNetworkPolicy(): NetworkPolicy {
  const stored = read(NETWORK_POLICY_KEY);
  if (!stored) return { mode: "off" };
  try {
    const value = JSON.parse(stored) as NetworkPolicy;
    if (
      value.mode === "off" ||
      value.mode === "package_managers" ||
      value.mode === "open"
    ) {
      return value;
    }
    if (
      value.mode === "allowed_hosts" &&
      Array.isArray(value.allowed_hosts) &&
      value.allowed_hosts.every((host) => typeof host === "string") &&
      typeof value.package_managers === "boolean"
    ) {
      return value;
    }
  } catch {
    // Fall through to the deny-by-default policy.
  }
  return { mode: "off" };
}

/**
 * What the next chat will be created with, chosen before it exists.
 *
 * The home composer carries the same controls as a conversation's, but there
 * is nothing to PATCH yet: the choices are held here and passed to `POST
 * /chats`, so a chat starts the way it was set up rather than being created
 * one way and corrected a moment later. That correcting PATCH is the failure
 * this avoids — it races the first turn, which reads the chat as it was
 * created.
 *
 * The choices persist, because "I work in Auto" is a standing preference, not
 * a per-visit one. They are deliberately not a global default applied to
 * chats created anywhere else: this is the state of one composer, and a chat
 * created from a project or a deep link has its own.
 */
export const useNewChatSettings = create<NewChatSettings>((set) => ({
  model: (read(MODEL_KEY) as ModelSelectionKey | null) ?? null,
  reasoningEffort: readEnum(REASONING_EFFORT_KEY, REASONING_EFFORTS),
  permissionMode: readEnum(PERMISSION_MODE_KEY, PERMISSION_MODES),
  networkPolicy: readNetworkPolicy(),
  setModel: (model) => {
    write(MODEL_KEY, model);
    set({ model });
  },
  setReasoningEffort: (reasoningEffort) => {
    write(REASONING_EFFORT_KEY, reasoningEffort);
    set({ reasoningEffort });
  },
  setPermissionMode: (permissionMode) => {
    write(PERMISSION_MODE_KEY, permissionMode);
    set({ permissionMode });
  },
  setNetworkPolicy: (networkPolicy) => {
    write(NETWORK_POLICY_KEY, JSON.stringify(networkPolicy));
    set({ networkPolicy });
  },
}));
