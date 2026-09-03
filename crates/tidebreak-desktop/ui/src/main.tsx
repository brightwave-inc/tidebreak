import React from "react";
import ReactDOM from "react-dom/client";
import { RouterProvider } from "@tanstack/react-router";
import { Toaster } from "@/components/ui/sonner";
import { restoreStoredAppMode } from "./appMode";
import { ErrorBoundary } from "./ErrorBoundary";
import { captureHandoffToken } from "./hostedSession";
import { refuseStrayFileDrops } from "./ImageAttachments";
import { createAppRouter } from "./router";
import { initTheme } from "./theme";
import "./styles.css";

initTheme();
restoreStoredAppMode();
refuseStrayFileDrops(window);
// Before the router: it owns the fragment from here on, and a handoff bearer
// left in it would be read as a route and stay in the address bar.
captureHandoffToken();
const router = createAppRouter();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <RouterProvider router={router} />
      <Toaster richColors />
    </ErrorBoundary>
  </React.StrictMode>,
);
