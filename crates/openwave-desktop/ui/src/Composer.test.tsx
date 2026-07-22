import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  boundedComposerHeight,
  Composer,
  MAX_COMPOSER_LINES,
  shouldRestoreComposerFocus,
  shouldSubmitComposerKey,
} from "./Composer";

const noop = async () => undefined;

describe("Composer", () => {
  it("submits only an unmodified Enter outside IME composition", () => {
    expect(
      shouldSubmitComposerKey({
        key: "Enter",
        shiftKey: false,
        ctrlKey: false,
        altKey: false,
        metaKey: false,
        isComposing: false,
        keyCode: 13,
      }),
    ).toBe(true);
    expect(
      shouldSubmitComposerKey({
        key: "Enter",
        shiftKey: true,
        ctrlKey: false,
        altKey: false,
        metaKey: false,
        isComposing: false,
        keyCode: 13,
      }),
    ).toBe(false);
    expect(
      shouldSubmitComposerKey({
        key: "Enter",
        shiftKey: false,
        ctrlKey: false,
        altKey: false,
        metaKey: false,
        isComposing: true,
        keyCode: 229,
      }),
    ).toBe(false);
    for (const modifier of ["ctrlKey", "altKey", "metaKey"] as const) {
      expect(
        shouldSubmitComposerKey({
          key: "Enter",
          shiftKey: false,
          ctrlKey: modifier === "ctrlKey",
          altKey: modifier === "altKey",
          metaKey: modifier === "metaKey",
          isComposing: false,
          keyCode: 13,
        }),
      ).toBe(false);
    }
  });

  it("caps auto-grow at six lines while retaining a one-line minimum", () => {
    const lineHeight = 20;
    const padding = 12;

    expect(boundedComposerHeight(0, lineHeight, padding)).toBe(32);
    expect(boundedComposerHeight(92, lineHeight, padding)).toBe(92);
    expect(boundedComposerHeight(500, lineHeight, padding)).toBe(
      MAX_COMPOSER_LINES * lineHeight + padding,
    );
  });

  it("does not restore focus into a newly selected conversation", () => {
    expect(shouldRestoreComposerFocus("chat-1", "chat-1", false)).toBe(true);
    expect(shouldRestoreComposerFocus("chat-1", "chat-2", false)).toBe(false);
    expect(shouldRestoreComposerFocus("chat-1", "chat-1", true)).toBe(false);
  });

  it("keeps one action slot and presents the stop control during a turn", () => {
    const markup = renderToStaticMarkup(
      <Composer
        activeTurnId="turn-1"
        busy
        cancelError={null}
        cancelPending={false}
        disabled={false}
        draft="A later draft"
        onDraftChange={vi.fn()}
        onSend={noop}
        onSteer={noop}
        onStop={noop}
        resetKey="chat-1"
        steerError={null}
        steerPending={false}
        steerStatus={null}
      />,
    );

    expect(markup).toContain('class="composer-actions"');
    expect(markup).toContain("Redirect");
    expect(markup).toContain('aria-label="Stop response"');
    expect(markup).toContain('aria-label="Redirect active response"');
    expect(markup).toContain('aria-label="Stop response"');
    expect(markup).toContain('aria-label="Message"');
    expect(markup).not.toContain('<textarea disabled=""');
    expect(markup).toContain('role="status"');
  });

  it("keeps Stop available for an active turn until the user types", () => {
    const markup = renderToStaticMarkup(
      <Composer
        activeTurnId="turn-1"
        busy
        cancelError={null}
        cancelPending={false}
        disabled={false}
        draft=""
        onDraftChange={vi.fn()}
        onSend={noop}
        onSteer={noop}
        onStop={noop}
        resetKey="chat-1"
        steerError={null}
        steerPending={false}
        steerStatus={null}
      />,
    );

    expect(markup).toContain('aria-label="Stop response"');
    expect(markup).toContain('aria-label="Stop response"');
    expect(markup).toContain("Guide the active response…");
    expect(markup).not.toContain('<textarea disabled=""');
  });

  it("keeps the draft editable while fencing an in-flight steer", () => {
    const markup = renderToStaticMarkup(
      <Composer
        activeTurnId="turn-1"
        busy
        cancelError={null}
        cancelPending={false}
        disabled={false}
        draft="change course"
        onDraftChange={vi.fn()}
        onSend={noop}
        onSteer={noop}
        onStop={noop}
        resetKey="chat-1"
        steerError={null}
        steerPending
        steerStatus="Sending guidance…"
      />,
    );

    expect(markup).toContain(">Sending…<");
    expect(markup).toContain('aria-label="Stop response"');
    expect(markup).toContain('aria-label="Redirect active response"');
    expect(markup).not.toContain('<textarea disabled=""');
    expect(markup).toContain("Sending guidance…");
  });

  it("keeps Stop available but fences Redirect while cancellation is pending", () => {
    const markup = renderToStaticMarkup(
      <Composer
        activeTurnId="turn-1"
        busy
        cancelError={null}
        cancelPending
        disabled={false}
        draft="change course"
        onDraftChange={vi.fn()}
        onSend={noop}
        onSteer={noop}
        onStop={noop}
        resetKey="chat-1"
        steerError={null}
        steerPending={false}
        steerStatus={null}
      />,
    );

    expect(markup).toContain(">Redirect<");
    expect(markup).toContain('aria-label="Stopping response"');
    expect(markup.match(/disabled=""/g)).toHaveLength(2);
  });

  it("surfaces unsupported active-turn guidance instead of silently ignoring it", () => {
    const markup = renderToStaticMarkup(
      <Composer
        activeTurnId="turn-1"
        busy
        cancelError={null}
        cancelPending={false}
        disabled={false}
        draft={"change\0course"}
        onDraftChange={vi.fn()}
        onSend={noop}
        onSteer={noop}
        onStop={noop}
        resetKey="chat-1"
        steerError={null}
        steerPending={false}
        steerStatus={null}
      />,
    );

    expect(markup).toContain("Guidance contains an unsupported character.");
    expect(markup).toContain('aria-label="Redirect active response"');
    expect(markup.match(/disabled=""/g)).toHaveLength(1);
  });

  it("disables an empty draft without hiding the send control", () => {
    const markup = renderToStaticMarkup(
      <Composer
        activeTurnId={null}
        busy={false}
        cancelError={null}
        cancelPending={false}
        disabled={false}
        draft="   "
        onDraftChange={vi.fn()}
        onSend={noop}
        onSteer={noop}
        onStop={noop}
        resetKey="chat-1"
        steerError={null}
        steerPending={false}
        steerStatus={null}
      />,
    );

    expect(markup).toContain('aria-label="Send message"');
    expect(markup).toContain('type="submit"');
    expect(markup).toContain('disabled=""');
  });
});
