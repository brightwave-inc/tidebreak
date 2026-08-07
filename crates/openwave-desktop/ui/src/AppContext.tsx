import { createContext, useContext } from "react";

import type {
  ApiClient,
  Chat,
  ModelInfo,
  Project,
  ProviderInfo,
} from "./api";
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
  /** Reload the chat list after a failed fetch — the rail's retry. */
  refreshChats: () => Promise<void>;
  status: string;
  setStatus: (next: string | ((current: string) => string)) => void;
  /** Start a conversation and open it. Fenced against a second in-flight create. */
  newChat: () => void;
  /** Confirm, then delete, then land somewhere real. */
  deleteChat: (chat: Chat) => void;
  startRename: (chat: Chat) => void;
  commitRename: (chat: Chat) => void;
  cancelRename: () => void;
  /** Create a project and drop its rail row straight into its rename field. */
  newProject: () => void;
  /**
   * Confirm, move the project's conversations back to Recents, then delete it.
   * The conversations survive: deleting a folder is not deleting its contents.
   */
  deleteProject: (project: Project) => void;
  startProjectRename: (project: Project) => void;
  commitProjectRename: (project: Project) => void;
  cancelProjectRename: () => void;
  /** Start a conversation inside a project and open it there. */
  newChatInProject: (projectId: string) => void;
  /** File a conversation under a project, or take it back out with `null`. */
  moveChatToProject: (chat: Chat, projectId: string | null) => void;
  updateState: DesktopUpdateState;
  /** The most recent explicit update check confirmed the app is current. */
  updateUpToDate: boolean;
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
