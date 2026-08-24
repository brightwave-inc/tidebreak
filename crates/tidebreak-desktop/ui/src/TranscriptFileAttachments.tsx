import { DocumentIcon } from "./components/document-table/DocumentIcon";
import { useSourceNav } from "./panel/SourceNav";

export type TranscriptFileAttachment = {
  documentId: string;
  name: string;
  mediaType: string;
};

export function TranscriptFileAttachments({
  files,
}: {
  files: readonly TranscriptFileAttachment[];
}) {
  const navigation = useSourceNav();
  return (
    <ul
      className="m-0 flex list-none flex-wrap gap-2 p-0"
      aria-label="Attached files"
    >
      {files.map((file) => (
        <li key={file.documentId}>
          <button
            type="button"
            className="flex max-w-full items-center gap-2 rounded-lg border border-border bg-background/70 px-3 py-2 text-left text-xs text-foreground transition-colors hover:bg-accent disabled:cursor-default disabled:opacity-60 disabled:hover:bg-background/70 sm:max-w-64"
            aria-label={`Open ${file.name}`}
            disabled={!navigation}
            onClick={() => navigation?.openDocument(file.documentId)}
          >
            <DocumentIcon mediaType={file.mediaType} className="size-4" />
            <span className="truncate" title={file.name}>
              {file.name}
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}
