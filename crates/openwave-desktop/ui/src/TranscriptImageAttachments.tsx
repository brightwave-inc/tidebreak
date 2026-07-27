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
        <TranscriptImage
          key={image.attachmentId}
          client={client}
          chatId={chatId}
          image={image}
          index={index}
        />
      ))}
    </div>
  );
}

function TranscriptImage({
  client,
  chatId,
  image,
  index,
}: {
  client: Pick<ApiClient, "getChatImageAttachment">;
  chatId: string;
  image: TranscriptImageAttachment;
  index: number;
}) {
  const [url, setUrl] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const label = `Attached image ${index + 1}: ${image.width} by ${image.height} pixels`;

  useEffect(() => {
    const abort = new AbortController();
    let objectUrl: string | null = null;
    setUrl(null);
    setUnavailable(false);

    void client
      .getChatImageAttachment(chatId, image.attachmentId, abort.signal)
      .then((blob) => {
        if (abort.signal.aborted) return;
        if (blob.type !== image.mediaType) {
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
  }, [client, chatId, image.attachmentId, image.mediaType]);

  if (url !== null) {
    return (
      <img
        className="message-image"
        src={url}
        alt={label}
        width={image.width}
        height={image.height}
      />
    );
  }

  return (
    <div className="message-image-pending" role="status" aria-label={label}>
      {unavailable ? "Attached image unavailable" : "Loading attached image…"}
    </div>
  );
}
