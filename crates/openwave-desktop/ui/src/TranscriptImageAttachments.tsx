import { useEffect, useState } from "react";
import type { ApiClient } from "./api";
import type { TranscriptImageAttachment } from "./ImageAttachments";

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
          label={`Attached image ${index + 1}: ${image.width} by ${image.height} pixels`}
          unavailableLabel="Attached image unavailable"
        />
      ))}
    </div>
  );
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
      <img
        className="message-image"
        src={url}
        alt={label}
        width={width}
        height={height}
      />
    );
  }

  return (
    <div className="message-image-pending" role="status" aria-label={label}>
      {unavailable ? unavailableLabel : "Loading image…"}
    </div>
  );
}
