import { Component, type ReactNode } from "react";
import { Logomark } from "./Logomark";

type ErrorBoundaryProps = {
  children: ReactNode;
  /** Injectable for tests; defaults to a real page reload. */
  onReload?: () => void;
  /**
   * Rendered instead of the full-page recovery screen.
   *
   * Supplied when the boundary wraps one part of the page rather than the
   * whole tree: a transcript row that cannot render should degrade to a note
   * in its own place, not replace the conversation around it with a reload
   * prompt.
   */
  fallback?: ReactNode;
};

type ErrorBoundaryState = {
  error: Error | null;
};

/**
 * Last-resort catch for render and lifecycle throws anywhere in the tree.
 * Without it a single throw unmounts everything into a blank window. Chat
 * data is durable in the embedded server, so a reload is a safe recovery.
 */
export class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: { componentStack?: string | null }) {
    console.error("unhandled render error", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    if (this.props.fallback !== undefined) return this.props.fallback;
    return (
      <div className="boot" role="alert">
        <div className="boot-brand">
          <Logomark />
          <h1>OpenWave</h1>
        </div>
        <p>OpenWave hit an unexpected error.</p>
        <p className="boot-error-detail">{String(this.state.error.message)}</p>
        <button
          type="button"
          className="btn btn-primary"
          onClick={() =>
            this.props.onReload
              ? this.props.onReload()
              : window.location.reload()
          }
        >
          Reload
        </button>
      </div>
    );
  }
}
