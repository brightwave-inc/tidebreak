import { createContext, useContext } from "react";

import type { ApiClient, Chat, ModelInfo, ProviderInfo } from "./api";
import type { ThemeMode } from "./theme";
import type { DesktopUpdateState } from "./updates";

/**
 * What the shell owns and every route needs: the connected client, the model
 * and provider catalog, and the status line the chat route keeps up to date.
 *
 * Routes are only mounted once the shell has a client, so this is never null
 * inside one — which is why {@link useApp} throws rather than returning an
 * optional the caller would have to narrow at every use.
 *
 * The chat mutations are here rather than drilled because the rails that invoke
 * them are owned by the routes now, not by the shell. Their orchestration —
 * the confirm dialog, the in-flight fences, what to open after a delete — still
 * belongs to the shell, which is the only thing that outlives every route.
 */
export type AppContextValue = {
  client: ApiClient;
  models: ModelInfo[];
  /**
   * The catalog key a chat without an override runs against, so the picker can
   * name its default rather than merely offering it. `null` when the server's
   * fallback is nothing the catalog names.
   */
  defaultModelKey: string | null;
  providers: ProviderInfo[];
  refreshCatalog: () => Promise<void>;
  status: string;
  setStatus: (next: string | ((current: string) => string)) => void;
  /** Start a conversation and open it. Fenced against a second in-flight create. */
  newChat: () => void;
  /** Confirm, then delete, then land somewhere real. */
  deleteChat: (chat: Chat) => void;
  startRename: (chat: Chat) => void;
  commitRename: (chat: Chat) => void;
  cancelRename: () => void;
  themeMode: ThemeMode;
  setThemeMode: (mode: ThemeMode) => void;
  cycleTheme: () => void;
  updateState: DesktopUpdateState;
  checkForUpdate: () => Promise<DesktopUpdateState>;
  restartForUpdate: () => Promise<void>;
};

const AppContext = createContext<AppContextValue | null>(null);

export const AppContextProvider = AppContext.Provider;

export function useApp(): AppContextValue {
  const value = useContext(AppContext);
  if (!value) throw new Error("useApp must be used inside the app shell");
  return value;
}
