import React from "react";
import ReactDOM from "react-dom/client";
import { RouterProvider } from "@tanstack/react-router";
import { Toaster } from "@/components/ui/sonner";
import { ErrorBoundary } from "./ErrorBoundary";
import { refuseStrayFileDrops } from "./ImageAttachments";
import { startImportQueue } from "./ImportQueueStore";
import { router } from "./router";
import { initTheme } from "./theme";
import "./styles.css";

initTheme();
refuseStrayFileDrops(window);
startImportQueue();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <RouterProvider router={router} />
      <Toaster richColors />
    </ErrorBoundary>
  </React.StrictMode>,
);
