import { useNavigate } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import { useChatListStore } from "./ChatListStore";
import { SettingsView } from "./SettingsView";

/**
 * Settings, as a place with an address.
 *
 * The conversation unmounts while this is open, which it can afford to: the
 * transcript is a view of a durable journal and rehydrates on the way back, and
 * the watcher that notices the agent asking a question lives in the shell, not
 * in the conversation. Going back returns to the chat that was open.
 */
export function SettingsRoute() {
  const navigate = useNavigate();
  const {
    client,
    models,
    providers,
    refreshCatalog,
    themeMode,
    setThemeMode,
    updateState,
    checkForUpdate,
    restartForUpdate,
  } = useApp();
  const openChatId = useChatListStore((state) => state.selected?.id ?? null);

  return (
    <SettingsView
      client={client}
      models={models}
      providers={providers}
      onProvidersChanged={() => void refreshCatalog()}
      onBack={() => {
        void (openChatId
          ? navigate({ to: "/c/$chatId", params: { chatId: openChatId } })
          : navigate({ to: "/" }));
      }}
      themeMode={themeMode}
      onThemeChange={setThemeMode}
      updateState={updateState}
      onCheckForUpdate={checkForUpdate}
      onRestartForUpdate={restartForUpdate}
    />
  );
}
