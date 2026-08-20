import { useEffect, useState } from "react";
import type { ApiClient } from "./api";
import type { TranscriptImageAttachment } from "./ImageAttachments";
import { cn } from "./lib/utils";

type TranscriptImageAttachmentsProps = {
  client: Pick<ApiClient, "getChatImageAttachment">;
  chatId: string;
  images: readonly TranscriptImageAttachment[];
};

/** Fetch transcript pixels with the renderer bearer, never through an image URL. */
export function TranscriptImageAttachments({
  client,
  chatId,
  images,
}: TranscriptImageAttachmentsProps) {
  return (
    <div className="message-image-grid" aria-label="Attached images">
      {images.map((image, index) => (
        <ChatImage
          key={image.attachmentId}
          client={client}
          chatId={chatId}
          attachmentId={image.attachmentId}
          mediaType={image.mediaType}
          width={image.width}
          height={image.height}
          label={imageLabel(index + 1, image.width, image.height)}
          unavailableLabel="Attached image unavailable"
        />
      ))}
    </div>
  );
}

function imageLabel(ordinal: number, width: number, height: number): string {
  if (width > 0 && height > 0) {
    return `attached image ${ordinal}: ${width} by ${height} pixels`;
  }
  return `attached image ${ordinal}`;
}

export function ChatImage({
  client,
  chatId,
  attachmentId,
  mediaType,
  width,
  height,
  label,
  unavailableLabel,
}: {
  client: Pick<ApiClient, "getChatImageAttachment">;
  chatId: string;
  attachmentId: string;
  mediaType: string;
  width: number;
  height: number;
  label: string;
  unavailableLabel: string;
}) {
  const [url, setUrl] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    const abort = new AbortController();
    let objectUrl: string | null = null;
    setUrl(null);
    setUnavailable(false);

    void client
      .getChatImageAttachment(chatId, attachmentId, abort.signal)
      .then((blob) => {
        if (abort.signal.aborted) return;
        if (blob.type !== mediaType) {
          setUnavailable(true);
          return;
        }
        objectUrl = URL.createObjectURL(blob);
        setUrl(objectUrl);
      })
      .catch(() => {
        if (!abort.signal.aborted) setUnavailable(true);
      });

    return () => {
      abort.abort();
      if (objectUrl !== null) URL.revokeObjectURL(objectUrl);
    };
  }, [client, chatId, attachmentId, mediaType]);

  if (url !== null) {
    return (
      <button
        type="button"
        className={cn("message-image-toggle", expanded && "is-expanded")}
        aria-expanded={expanded}
        aria-label={expanded ? `Collapse ${label}` : `Expand ${label}`}
        onClick={() => setExpanded((open) => !open)}
      >
        <img
          className="message-image"
          src={url}
          alt=""
          {...(width > 0 ? { width } : {})}
          {...(height > 0 ? { height } : {})}
        />
      </button>
    );
  }

  return (
    <div className="message-image-pending" role="status" aria-label={label}>
      {unavailable ? unavailableLabel : "Loading image…"}
    </div>
  );
}
