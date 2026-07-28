import { useNavigate, useRouter } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import { RouteFrame } from "./RouteFrame";
import { HomeSidebar } from "./sidebar/HomeSidebar";
import { SettingsView } from "./SettingsView";

/**
 * Settings, as a place with an address.
 *
 * The conversation unmounts while this is open, which it can afford to: the
 * transcript is a view of a durable journal and rehydrates on the way back, and
 * the watcher that notices the agent asking a question lives in the shell, not
 * in the conversation.
 *
 * Going back is the previous history entry rather than a remembered chat id.
 * Settings is reachable from everywhere, so where the reader came from is the
 * only answer that is right from all of them — and it is one fewer place that
 * has to keep its own copy of which conversation is open.
 */
export function SettingsRoute() {
  const navigate = useNavigate();
  const router = useRouter();
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

  return (
    <RouteFrame sidebar={<HomeSidebar />}>
      <SettingsView
        client={client}
        models={models}
        providers={providers}
        onProvidersChanged={() => void refreshCatalog()}
        onBack={() => {
          // A deep link straight to settings has nothing behind it.
          if (router.history.canGoBack()) router.history.back();
          else void navigate({ to: "/" });
        }}
        themeMode={themeMode}
        onThemeChange={setThemeMode}
        updateState={updateState}
        onCheckForUpdate={checkForUpdate}
        onRestartForUpdate={restartForUpdate}
      />
    </RouteFrame>
  );
}
