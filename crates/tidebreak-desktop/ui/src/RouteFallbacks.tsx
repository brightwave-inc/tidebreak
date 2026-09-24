import {
  type ErrorComponentProps,
  type NotFoundRouteProps,
  useRouter,
  useRouterState,
} from "@tanstack/react-router";
import { Compass, House, RotateCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Notice, NoticeDetail } from "@/components/ui/notice";
import { friendlyErrorMessage } from "@/lib/utils";
import { CrashScreen, homePathFor, useCopyCrashReport } from "./CrashScreen";
import { reportRendererError } from "./rendererErrors";
import { RouteFrame, useInsideRouteFrame } from "./RouteFrame";
import { AppSidebar } from "./sidebar/AppSidebar";
import { PaneDragBand } from "./WindowDragStrip";

/**
 * Route crashes reach this machine's log the way the app-wide boundary's do:
 * the router catches them first, so the boundary never sees them.
 */
export function reportRouteError(
  error: unknown,
  info: { componentStack?: string | null },
): void {
  reportRendererError("render", error, {
    componentStack: info.componentStack,
  });
}

/**
 * The router's default error screen: a crash in the shell or in a layout,
 * where no rail survived to keep. It takes the window, like the app-wide
 * boundary does.
 */
export function RouteCrashScreen({ error, info }: ErrorComponentProps) {
  const router = useRouter();
  return (
    <CrashScreen
      error={error}
      componentStack={info?.componentStack}
      onReload={() => window.location.reload()}
      onGoHome={() =>
        void router.navigate({
          to: homePathFor(router.state.location.pathname),
        })
      }
    />
  );
}

/**
 * A page that crashed inside its frame. The pane says so and the rail beside
 * it keeps working, so the reader can try the page again or go anywhere else
 * without reloading the window.
 */
export function RoutePaneError({ error, reset, info }: ErrorComponentProps) {
  const router = useRouter();
  const report = useCopyCrashReport({
    error,
    componentStack: info?.componentStack,
  });
  return (
    <div className="relative flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-y-auto">
      <PaneDragBand />
      <div className="mx-auto w-full max-w-2xl px-6 pt-10 pb-6">
        <Notice
          tone="critical"
          title={<h1>This page hit an unexpected error</h1>}
          action={
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                reset();
                void router.invalidate();
              }}
            >
              <RotateCw aria-hidden="true" />
              Try again
            </Button>
          }
        >
          <p>
            Try the page again. If it keeps failing, reload the window or copy
            the debug info for a bug report.
          </p>
          <NoticeDetail>
            {friendlyErrorMessage(error, "No error message was recorded.")}
          </NoticeDetail>
        </Notice>
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <Button
            size="sm"
            variant="ghost"
            onClick={() =>
              void router.navigate({
                to: homePathFor(router.state.location.pathname),
              })
            }
          >
            <House aria-hidden="true" />
            Go home
          </Button>
          <Button size="sm" variant="ghost" onClick={() => void report.copy()}>
            {report.label}
          </Button>
        </div>
      </div>
    </div>
  );
}

/**
 * An address no route answers: an old deep link, a mistyped hosted URL.
 *
 * Inside a frame (an unknown settings section) it fills the pane beside that
 * frame's rail. Under the shell (an unknown path) it brings the Work rail, so
 * the reader is never left on a page with no way out.
 */
export function RouteNotFound(_props: NotFoundRouteProps) {
  const framed = useInsideRouteFrame();
  const page = <NotFoundPane />;
  return framed ? (
    page
  ) : (
    <RouteFrame sidebar={<AppSidebar />}>{page}</RouteFrame>
  );
}

function NotFoundPane() {
  const router = useRouter();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const home = homePathFor(pathname);
  return (
    <div className="relative flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-y-auto">
      <PaneDragBand />
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <Compass />
          </EmptyMedia>
          <EmptyTitle>
            <h1>This page does not exist</h1>
          </EmptyTitle>
          <EmptyDescription>
            The link may be out of date, or the page may have moved. Go home to
            pick up where you left off.
          </EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <Button
            size="sm"
            variant="outline"
            onClick={() => void router.navigate({ to: home })}
          >
            <House aria-hidden="true" />
            {home === "/code" ? "Go to Code home" : "Go home"}
          </Button>
        </EmptyContent>
      </Empty>
    </div>
  );
}
