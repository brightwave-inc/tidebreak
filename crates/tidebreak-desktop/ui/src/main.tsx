import React from "react";
import ReactDOM from "react-dom/client";
import { RouterProvider } from "@tanstack/react-router";
import { Toaster } from "@/components/ui/sonner";
import { restoreStoredAppMode } from "./appMode";
import { ErrorBoundary } from "./ErrorBoundary";
import { refuseStrayFileDrops } from "./ImageAttachments";
import { createAppRouter } from "./router";
import { initTheme } from "./theme";
import "katex/dist/katex.min.css";
import "./styles.css";

initTheme();
restoreStoredAppMode();
refuseStrayFileDrops(window);
const router = createAppRouter();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <RouterProvider router={router} />
      <Toaster richColors />
    </ErrorBoundary>
  </React.StrictMode>,
);
