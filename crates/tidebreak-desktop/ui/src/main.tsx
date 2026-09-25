import React from "react";
import ReactDOM from "react-dom/client";
import { RouterProvider } from "@tanstack/react-router";
import { Toaster } from "@/components/ui/sonner";
import { restoreStoredAppMode } from "./appMode";
import { DesktopQuitPromptHost } from "./DesktopQuitPromptHost";
import { ErrorBoundary } from "./ErrorBoundary";
import { hydrateComposerDraftFromHostedReentry } from "./ComposerDrafts";
import { captureHandoffToken } from "./hostedSession";
import { refuseStrayFileDrops } from "./ImageAttachments";
import { ReportProblemHost } from "./ReportProblemDialog";
import { rendererErrors } from "./rendererErrors";
import { createAppRouter } from "./router";
import { initTheme } from "./theme";
import "./styles.css";

// First, so an error anywhere after this reaches the log once boot knows
// where the server is.
rendererErrors.install(window);
initTheme();
restoreStoredAppMode();
refuseStrayFileDrops(window);
// Before the router: it owns the fragment from here on, and a handoff bearer
// left in it would be read as a route and stay in the address bar.
captureHandoffToken();
hydrateComposerDraftFromHostedReentry();
const router = createAppRouter();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <RouterProvider router={router} />
      <Toaster richColors />
    </ErrorBoundary>
    {/* Outside the app's boundary, so a page that crashed can still quit. */}
    <ErrorBoundary fallback={null}>
      <DesktopQuitPromptHost
        onOpenInbox={() => void router.navigate({ to: "/code/inbox" })}
      />
    </ErrorBoundary>
    {/* Outside it too, so a failed boot or a crash can report a problem. */}
    <ErrorBoundary fallback={null}>
      <ReportProblemHost />
    </ErrorBoundary>
  </React.StrictMode>,
);
