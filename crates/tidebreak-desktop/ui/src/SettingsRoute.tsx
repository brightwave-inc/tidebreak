import { Outlet, useNavigate, useRouter } from "@tanstack/react-router";

import { RouteFrame } from "./RouteFrame";
import { SettingsSidebar } from "./sidebar/SettingsSidebar";

/**
 * Settings, as a place with an address.
 *
 * This is the layout the sections hang in: it owns the rail and the way back,
 * and renders whichever section the URL names into the outlet. Because each
 * section has its own path, a visit lands where the reader left off rather than
 * resetting, and a link can be shared straight to it.
 *
 * The conversation unmounts while this is open, which it can afford to: the
 * transcript is a view of a durable journal and rehydrates on the way back, and
 * the watcher that notices the agent asking a question lives in the shell, not
 * in the conversation.
 *
 * Going back is the previous history entry rather than a remembered chat id.
 * Settings is reachable from everywhere, so where the reader came from is the
 * only answer that is right from all of them — and it is one fewer place that
 * has to keep its own copy of which conversation is open. That entry is always
 * the one before settings because navigation inside settings — the index
 * redirect and the rail's section links — replaces rather than pushes.
 */
export function SettingsRoute() {
  const navigate = useNavigate();
  const router = useRouter();

  return (
    <RouteFrame
      className="settings-route-frame"
      mainClassName="settings-main"
      sidebar={
        <SettingsSidebar
          onBack={() => {
            // A deep link straight to settings has nothing behind it.
            if (router.history.canGoBack()) router.history.back();
            else void navigate({ to: "/" });
          }}
        />
      }
    >
      <Outlet />
    </RouteFrame>
  );
}
