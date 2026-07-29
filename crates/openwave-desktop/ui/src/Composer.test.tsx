import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  boundedComposerHeight,
  Composer,
  imageSendBlocker,
  MAX_COMPOSER_LINES,
  shouldRestoreComposerFocus,
  shouldSubmitComposerKey,
  type ComposerImages,
} from "./Composer";
import {
  queuedImageAttachment,
  readyImageAttachment,
  withUploadFailed,
  withUploadProgress,
  withUploadStarted,
  type ImageAttachment,
} from "./ImageAttachments";

const noop = async () => undefined;

function images(overrides: Partial<ComposerImages> = {}): ComposerImages {
  return {
    items: [],
    error: null,
    unsupportedModel: null,
    onAttachFiles: vi.fn(),
    onRemove: vi.fn(),
    onRetry: vi.fn(),
    ...overrides,
  };
}

function attached(id: string, name: string): ImageAttachment {
  return queuedImageAttachment(id, {
    name,
    byteLen: 1_000,
    previewUrl: `blob:${id}`,
  });
}

function composerWithImages(overrides: Partial<ComposerImages>): string {
  return renderToStaticMarkup(
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft="What is in this?"
      images={images(overrides)}
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
}

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

  it("offers one attach control, and reports what it added", () => {
    const markup = renderToStaticMarkup(
      <Composer
        activeTurnId={null}
        busy={false}
        cancelError={null}
        cancelPending={false}
        disabled={false}
        draft="Summarize this"
        canAttach
        attaching={false}
        attachedSourceName="brief.pdf"
        attachError={null}
        onAttach={noop}
        onDismissAttachedSource={vi.fn()}
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

    // One control for any file. Which of the two things it becomes is the
    // host's decision from the bytes, not a choice put to the reader — the
    // menu behind this trigger offers a single upload, never a per-kind pair.
    expect(markup).toContain('aria-label="Add to this chat"');
    expect(markup).not.toContain('aria-label="Attach image"');
    expect(markup).toContain("brief.pdf");
    expect(markup).toContain("Added to this conversation");
    expect(markup).toContain('aria-label="Dismiss brief.pdf"');
  });

  it("previews an uploading image from the local file with determinate progress", () => {
    const uploading = withUploadProgress(
      withUploadStarted([attached("a", "chart.png")], "a"),
      "a",
      250,
    );
    const markup = composerWithImages({ items: uploading });

    expect(markup).toContain('src="blob:a"');
    expect(markup).toContain("chart.png");
    expect(markup).toContain("Uploading 25%");
    expect(markup).toContain('<progress class="composer-image-progress"');
    expect(markup).toContain('max="100"');
    expect(markup).toContain('value="25"');
    expect(markup).toContain('aria-label="Uploading chart.png"');
    // Sending mid-upload would drop the image the reader is waiting for.
    expect(markup).toContain('aria-label="Send message" disabled=""');
  });

  it("keeps a failed image on screen with a way to try again", () => {
    const failed = withUploadFailed(
      withUploadStarted([attached("a", "chart.png")], "a"),
      "a",
      "That image file is damaged",
    );
    const markup = composerWithImages({ items: failed });

    expect(markup).toContain("chart.png");
    expect(markup).toContain("That image file is damaged");
    expect(markup).toContain("Try again");
    expect(markup).toContain('role="alert"');
    expect(markup).toContain('aria-label="Send message" disabled=""');
  });

  it("labels removal per image and never hides it behind a pointer", () => {
    const markup = composerWithImages({
      items: [
        attached("a", "chart.png"),
        readyImageAttachment("b", {
          attachmentId: "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21",
          fileName: "beach.png",
          mediaType: "image/png",
          width: 800,
          height: 600,
          byteLen: 2_048,
        }),
      ],
    });

    // A shared label leaves a screen reader with two identical controls.
    expect(markup).toContain('aria-label="Remove chart.png"');
    expect(markup).toContain('aria-label="Remove beach.png"');
    // The picker cannot show pixels the renderer never received, so a picked
    // image is identified by its name and geometry instead.
    expect(markup).toContain("beach.png");
    expect(markup).toContain("800 × 600");
  });

  it("says which model cannot read the attached image, and blocks the send", () => {
    const markup = composerWithImages({
      items: [
        readyImageAttachment("b", {
          attachmentId: "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21",
          fileName: "beach.png",
          mediaType: "image/png",
          width: 800,
          height: 600,
          byteLen: 2_048,
        }),
      ],
      unsupportedModel: "Local Model",
    });

    expect(markup).toContain("Local Model can’t read images.");
    expect(markup).toContain("Choose a model that");
    expect(markup).toContain("remove the attached image");
    expect(markup).toContain('aria-label="Send message" disabled=""');
  });

  it("explains why a turn carrying images is not sendable yet", () => {
    expect(imageSendBlocker(undefined)).toBeNull();
    expect(imageSendBlocker(images())).toBeNull();
    expect(
      imageSendBlocker(images({ items: [attached("a", "chart.png")] })),
    ).toBe("Waiting for images to upload");
    expect(
      imageSendBlocker(
        images({
          items: withUploadFailed([attached("a", "chart.png")], "a", "damaged"),
        }),
      ),
    ).toBe("An image did not upload");
    const ready = readyImageAttachment("b", {
      attachmentId: "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21",
      fileName: "beach.png",
      mediaType: "image/png",
      width: 800,
      height: 600,
      byteLen: 2_048,
    });
    expect(
      imageSendBlocker(
        images({ items: [ready], unsupportedModel: "Local Model" }),
      ),
    ).toBe("Local Model cannot read images");
    expect(imageSendBlocker(images({ items: [ready] }))).toBeNull();
    // A text-only model is only a problem for a turn that carries an image.
    expect(imageSendBlocker(images({ unsupportedModel: "Local Model" }))).toBeNull();
  });
});
