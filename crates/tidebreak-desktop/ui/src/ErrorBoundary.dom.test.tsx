// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ErrorBoundary } from "./ErrorBoundary";

function Bomb(): never {
  throw new Error("kaboom in render");
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("ErrorBoundary", () => {
  it("renders healthy children untouched", () => {
    render(
      <ErrorBoundary>
        <p>all good</p>
      </ErrorBoundary>,
    );
    expect(screen.getByText("all good")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("catches a render throw and shows the fallback with the message", () => {
    // React logs caught errors loudly; keep the test output readable.
    vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <ErrorBoundary>
        <Bomb />
      </ErrorBoundary>,
    );
    const fallback = screen.getByRole("alert");
    expect(fallback).toHaveTextContent("Tidebreak hit an unexpected error.");
    expect(fallback).toHaveTextContent("kaboom in render");
  });

  it("offers reload as recovery", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const user = userEvent.setup();
    const onReload = vi.fn();
    render(
      <ErrorBoundary onReload={onReload}>
        <Bomb />
      </ErrorBoundary>,
    );
    await user.click(screen.getByRole("button", { name: "Reload" }));
    expect(onReload).toHaveBeenCalledOnce();
  });
});

describe("ErrorBoundary with an inline fallback", () => {
  it("contains a throw to its own region instead of replacing the page", () => {
    const Boom = () => {
      throw new Error("tool row exploded");
    };
    render(
      <div>
        <p>the conversation around it</p>
        <ErrorBoundary fallback={<p>This step could not be displayed.</p>}>
          <Boom />
        </ErrorBoundary>
      </div>,
    );

    expect(screen.getByText("This step could not be displayed.")).toBeTruthy();
    // The point of the inline fallback: the rest of the transcript survives,
    // and the reader is not offered a reload for one bad row.
    expect(screen.getByText("the conversation around it")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Reload" })).toBeNull();
  });

  it("retries the children when their data changes, and not before", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    // Throws only while the result is half-written, the way a streaming row
    // meets a shape its parser cannot read yet.
    const Row = ({ result }: { result: string }) => {
      if (result === "partial") throw new Error("half-written result");
      return <p>{result}</p>;
    };
    const boundary = (result: string) => (
      <ErrorBoundary resetKey={result} fallback={<p>unavailable</p>}>
        <Row result={result} />
      </ErrorBoundary>
    );

    const { rerender } = render(boundary("partial"));
    expect(screen.getByText("unavailable")).toBeTruthy();

    // A re-render carrying the same data leaves it alone: retrying every frame
    // would throw on every frame.
    rerender(boundary("partial"));
    expect(screen.getByText("unavailable")).toBeTruthy();

    rerender(boundary("settled"));
    expect(screen.getByText("settled")).toBeTruthy();
  });
});
