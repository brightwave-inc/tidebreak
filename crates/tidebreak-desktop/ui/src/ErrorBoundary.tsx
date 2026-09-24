import { Component, type ReactNode } from "react";
import { CrashScreen, currentRoutePath, homePathFor } from "./CrashScreen";
import { reportRendererError } from "./rendererErrors";

type ErrorBoundaryProps = {
  children: ReactNode;
  /** Injectable for tests; defaults to a real page reload. */
  onReload?: () => void;
  /** Injectable for tests; defaults to reloading onto the home page. */
  onGoHome?: () => void;
  /**
   * Rendered instead of the full-page recovery screen.
   *
   * Supplied when the boundary wraps one part of the page rather than the
   * whole tree: a transcript row that cannot render should degrade to a note
   * in its own place, not replace the conversation around it with a reload
   * prompt.
   */
  fallback?: ReactNode;
  /**
   * Data signature of the children. A change clears a caught error and lets
   * them render again.
   *
   * A boundary latches: whatever threw stays replaced by the fallback for the
   * life of the mount, even though a streaming transcript re-renders around it
   * many times a second. Callers pass a signal derived from the data being
   * rendered, so a row that threw on a half-written result recovers once the
   * result changes, while a row that is simply malformed stays quiet instead of
   * throwing on every frame.
   */
  resetKey?: string | number;
};

type ErrorBoundaryState = {
  error: Error | null;
  componentStack: string | null;
};

/**
 * Outside the router there is no navigation to ask for, so going home is a
 * reload onto the home page's address.
 */
function reloadHome() {
  window.location.hash = `#${homePathFor(currentRoutePath())}`;
  window.location.reload();
}

/**
 * Last-resort catch for render and lifecycle throws anywhere in the tree.
 * Without it a single throw unmounts everything into a blank window. Chat
 * data is durable in the embedded server, so a reload is a safe recovery.
 * Every catch is also written to this machine's log; see `rendererErrors`.
 */
export class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { error: null, componentStack: null };

  static getDerivedStateFromError(error: Error): Partial<ErrorBoundaryState> {
    return { error };
  }

  componentDidCatch(error: Error, info: { componentStack?: string | null }) {
    console.error("unhandled render error", error, info.componentStack);
    this.setState({ componentStack: info.componentStack ?? null });
    reportRendererError("render", error, {
      componentStack: info.componentStack,
    });
  }

  componentDidUpdate(previous: ErrorBoundaryProps) {
    if (this.state.error === null) return;
    if (previous.resetKey === this.props.resetKey) return;
    this.setState({ error: null, componentStack: null });
  }

  render() {
    if (!this.state.error) return this.props.children;
    if (this.props.fallback !== undefined) return this.props.fallback;
    return (
      <CrashScreen
        error={this.state.error}
        componentStack={this.state.componentStack}
        onReload={() =>
          this.props.onReload ? this.props.onReload() : window.location.reload()
        }
        onGoHome={this.props.onGoHome ?? reloadHome}
      />
    );
  }
}
