/**
 * The presentation viewer: the original deck converted to PDF and drawn with
 * the PDF engine, behind a thin strip saying so.
 *
 * The conversion needs LibreOffice. On macOS the app installs its own copy
 * the first time a preview needs one — visibly and cancellably, with a
 * determinate download bar — an exact pinned version the host verifies
 * against a pinned digest before unpacking. A failed or cancelled install
 * turns into the install hint with the reason and an explicit retry; nothing
 * re-downloads on its own. Platforms without a managed install keep the
 * install-it-yourself hint, and a failed conversion gets its own card so a
 * corrupt file never reads as a broken app.
 */
import {
  FileSpreadsheetIcon,
  Loader2Icon,
  PresentationIcon,
} from "lucide-react";
import { lazy, useEffect, useMemo, useRef, useState } from "react";

import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import {
  cancelPresentationConverterInstall,
  ConverterMissingError,
  installPresentationConverter,
  OfficeConversionError,
  officePdfSource,
  type ConverterInstallProgress,
} from "@/document/officePdf";
import {
  useFileDownload,
  type FileBytesSource,
} from "@/document/useFileDownload";
import { cn } from "@/lib/utils";

// The PDF engine is fetched on first use, same as the direct PDF branch.
const PdfViewer = lazy(() =>
  import("@/document/PdfViewer").then((m) => ({ default: m.PdfViewer })),
);

interface Props {
  /** The original presentation's bytes. */
  source: FileBytesSource;
  mediaType: string;
  className?: string;
}

export function PresentationViewer({ source, mediaType, className }: Props) {
  return (
    <ConvertedOfficeViewer
      source={source}
      mediaType={mediaType}
      kind="presentation"
      className={className}
    />
  );
}

interface ConvertedOfficeViewerProps extends Props {
  kind: "presentation" | "spreadsheet";
}

export function ConvertedOfficeViewer({
  source,
  mediaType,
  kind,
  className,
}: ConvertedOfficeViewerProps) {
  // Bumped when the managed install completes, so the conversion that found
  // no converter is retried under a fresh cache key.
  const [attempt, setAttempt] = useState(0);
  const pdfSource = useMemo(() => {
    const converted = officePdfSource(source, mediaType);
    return attempt === 0
      ? converted
      : { ...converted, cacheKey: `${converted.cacheKey}#${attempt}` };
    // The source object is rebuilt each render; its cache key is its identity.
  }, [source.cacheKey, mediaType, attempt]);
  const { data, isLoading, error } = useFileDownload(pdfSource, {
    parseAs: "arrayBuffer",
  });

  if (isLoading) {
    return (
      <div className="flex grow flex-col items-center justify-center gap-3">
        <Loader2Icon className="size-6 animate-spin text-muted-foreground" />
        <p className="text-sm text-muted-foreground" role="status">
          Preparing preview…
        </p>
      </div>
    );
  }

  if (error instanceof ConverterMissingError) {
    if (error.installable) {
      return (
        <ConverterInstall
          key={attempt}
          kind={kind}
          initialFailure={error.installFailure}
          onReady={() => setAttempt((current) => current + 1)}
        />
      );
    }
    return (
      <OfficeNotice kind={kind}>
        Install LibreOffice to preview this {kind}. The file itself is fine —
        Save as… exports it unchanged.
      </OfficeNotice>
    );
  }

  if (error !== null || data === null) {
    return (
      <OfficeNotice kind={kind}>
        <span className="block">
          This {kind} could not be converted for preview.
        </span>
        {error?.message ? (
          <span className="mt-1 block">{error.message}</span>
        ) : null}
        {error instanceof OfficeConversionError ? (
          <details className="mt-3 w-full text-left">
            <summary className="cursor-pointer text-xs font-medium text-foreground">
              Troubleshooting details
            </summary>
            <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md border bg-muted/40 p-3 font-mono text-xs text-foreground">
              {error.details}
            </pre>
          </details>
        ) : null}
        <span className="mt-1 block">Save as… exports the original file.</span>
      </OfficeNotice>
    );
  }

  return (
    <div className="flex min-h-0 grow flex-col">
      <p className="shrink-0 px-4 pt-2 text-xs text-muted-foreground">
        Converted PDF preview — Save as… exports the original file.
      </p>
      <PdfViewer source={pdfSource} className={cn("min-h-0", className)} />
    </div>
  );
}

/**
 * The managed LibreOffice install, as the preview panel sees it: starts on
 * its own unless an earlier attempt this run already failed, shows a
 * determinate download bar, and can be cancelled. Failure lands on the hint
 * with the reason; only the explicit retry starts another download.
 */
function ConverterInstall({
  kind,
  initialFailure,
  onReady,
}: {
  kind: "presentation" | "spreadsheet";
  initialFailure: string | null;
  onReady: () => void;
}) {
  const [failure, setFailure] = useState<string | null>(initialFailure);
  const [running, setRunning] = useState(initialFailure === null);
  const [progress, setProgress] = useState<ConverterInstallProgress | null>(
    null,
  );
  const onReadyRef = useRef(onReady);
  onReadyRef.current = onReady;

  useEffect(() => {
    if (!running) return;
    // The install itself is a shared module-level operation, so effect
    // re-runs (StrictMode mounts every effect twice) simply join the one
    // in-flight install; `disposed` only keeps a dead mount's state setters
    // quiet. The surviving run receives progress and completion regardless of
    // which run started the install.
    let disposed = false;
    void installPresentationConverter((next) => {
      if (!disposed) setProgress(next);
    })
      .then(() => {
        if (!disposed) onReadyRef.current();
      })
      .catch((error: unknown) => {
        if (disposed) return;
        setFailure(error instanceof Error ? error.message : String(error));
        setRunning(false);
        setProgress(null);
      });
    return () => {
      disposed = true;
    };
  }, [running]);

  if (!running) {
    return (
      <OfficeNotice kind={kind}>
        <span className="block">
          Couldn’t set up the {kind} preview
          {failure ? `: ${failure}` : "."}
        </span>
        <span className="mt-1 block">
          The file itself is fine — Save as… exports it unchanged. You can also
          install LibreOffice yourself.
        </span>
        <Button
          variant="outline"
          size="sm"
          className="mt-3"
          onClick={() => {
            setFailure(null);
            setRunning(true);
          }}
        >
          Try again
        </Button>
      </OfficeNotice>
    );
  }

  const downloading = progress?.phase !== "installing";
  const percent =
    progress?.phase === "downloading" && progress.totalBytes
      ? Math.min(100, (progress.downloadedBytes / progress.totalBytes) * 100)
      : null;

  return (
    <div className="flex grow items-center justify-center p-6">
      <div className="flex w-full max-w-sm flex-col items-center gap-3 text-center">
        <OfficeIcon kind={kind} />
        <p className="text-sm text-muted-foreground" role="status">
          {downloading
            ? `Preparing ${kind} preview — downloading LibreOffice (~300 MB)…`
            : "Setting up LibreOffice…"}
        </p>
        <Progress value={percent ?? undefined} className="w-56" />
        {percent !== null && progress?.totalBytes ? (
          <p className="text-xs text-muted-foreground">
            {formatMegabytes(progress.downloadedBytes)} of{" "}
            {formatMegabytes(progress.totalBytes)} MB
          </p>
        ) : null}
        {downloading ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void cancelPresentationConverterInstall()}
          >
            Cancel
          </Button>
        ) : null}
      </div>
    </div>
  );
}

function formatMegabytes(bytes: number): string {
  return Math.round(bytes / (1024 * 1024)).toString();
}

function OfficeIcon({ kind }: { kind: "presentation" | "spreadsheet" }) {
  const Icon = kind === "presentation" ? PresentationIcon : FileSpreadsheetIcon;
  return <Icon className="size-6 text-muted-foreground" />;
}

function OfficeNotice({
  kind,
  children,
}: {
  kind: "presentation" | "spreadsheet";
  children: React.ReactNode;
}) {
  return (
    <div className="flex grow items-center justify-center p-6">
      <div className="flex w-full max-w-xl flex-col items-center gap-3 text-center">
        <OfficeIcon kind={kind} />
        <p className="text-sm text-muted-foreground" role="status">
          {children}
        </p>
      </div>
    </div>
  );
}
