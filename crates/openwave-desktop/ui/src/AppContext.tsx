import { createContext, useContext } from "react";

import type { ApiClient, ModelInfo, ProviderInfo } from "./api";
import type { ThemeMode } from "./theme";
import type { DesktopUpdateState } from "./updates";

/**
 * What the shell owns and every route needs: the connected client, the model
 * and provider catalog, and the status line the chat route keeps up to date.
 *
 * Routes are only mounted once the shell has a client, so this is never null
 * inside one — which is why {@link useApp} throws rather than returning an
 * optional the caller would have to narrow at every use.
 */
export type AppContextValue = {
  client: ApiClient;
  models: ModelInfo[];
  providers: ProviderInfo[];
  refreshCatalog: () => Promise<void>;
  status: string;
  setStatus: (next: string | ((current: string) => string)) => void;
  themeMode: ThemeMode;
  setThemeMode: (mode: ThemeMode) => void;
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
