import { renderAsync } from "docx-preview";
import { Loader2Icon } from "lucide-react";
import type { HTMLAttributes } from "react";
import { useEffect, useState } from "react";

import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import { cn } from "@/lib/utils";
import { useFileDownload, type FileBytesSource } from "./useFileDownload";

/**
 * docx-preview intentionally renders the Word package into ordinary HTML.
 * altChunk stays disabled so a DOCX cannot add arbitrary HTML to that output.
 * The generated HTML and CSS are additionally confined to an opaque-origin,
 * scriptless iframe below.
 */
export const DOCX_RENDER_OPTIONS = {
  breakPages: true,
  // The experimental tab-stop pass runs on a fixed delayed timer after
  // renderAsync resolves. Keep it off so serialization never races it.
  experimental: false,
  ignoreLastRenderedPageBreak: false,
  renderAltChunks: false,
  renderComments: false,
  renderEndnotes: true,
  renderFooters: true,
  renderFootnotes: true,
  renderHeaders: true,
  useBase64URL: true,
} as const;

const DOCX_FRAME_PREFIX = `<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: blob:; font-src data: blob:; style-src 'unsafe-inline'; script-src 'none'; connect-src 'none'; form-action 'none'; base-uri 'none'; frame-src 'none'; object-src 'none'">
  <meta name="referrer" content="no-referrer">
  <style>
    :root { color-scheme: light; }
    * { box-sizing: border-box; }
    html, body { min-height: 100%; margin: 0; }
    body {
      min-width: 0;
      overflow: auto;
      background: rgb(241 245 249);
      font-family: system-ui, sans-serif;
    }
    #document-root { min-height: 100%; }
    #document-root .docx-wrapper {
      min-height: 100%;
      padding: 24px !important;
      background: rgb(241 245 249) !important;
    }
    #document-root section.docx {
      height: auto !important;
      min-height: var(--tidebreak-docx-page-height);
      margin: 0 auto 24px !important;
      overflow: visible !important;
      color: #111827;
      box-shadow: 0 1px 2px rgb(15 23 42 / 0.12), 0 8px 28px rgb(15 23 42 / 0.10) !important;
    }
    #document-root section.docx:last-child { margin-bottom: 0 !important; }
  </style>
</head>
<body>
  <div id="document-root">`;

const DOCX_FRAME_SUFFIX = `</div>
</body>
</html>`;

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
}

/** A read-only DOCX viewer that renders the original Word package locally. */
export default function DocxViewer({
  source,
  className,
  ...restProps
}: Props) {
  const [frameDocument, setFrameDocument] = useState<string | null>(null);
  const [errorType, setErrorType] = useState<"parse" | "load" | null>(null);
  const [isReady, setIsReady] = useState(false);

  const fileDownload = useFileDownload(source, { parseAs: "arrayBuffer" });

  useEffect(() => {
    if (!fileDownload.data) return;

    let cancelled = false;
    const stagingHost = document.createElement("div");
    const staging = document.createElement("div");
    stagingHost.style.cssText =
      "position:fixed;left:-100000px;top:0;width:1200px;opacity:0;pointer-events:none;";
    stagingHost.attachShadow({ mode: "closed" }).append(staging);
    document.body.append(stagingHost);
    setFrameDocument(null);
    setIsReady(false);
    setErrorType(null);

    // docx-preview cannot cancel a parse. Render inside an offscreen closed
    // shadow root so layout-dependent SVG sizing can finish without generated
    // styles entering Tidebreak's main document. The sanitized serialization
    // is then loaded into a scriptless sandboxed frame.
    void renderAsync(fileDownload.data, staging, staging, DOCX_RENDER_OPTIONS)
      .then(async () => {
        await waitForLayoutPass();
        if (cancelled) return;
        sanitizeRenderedDocument(staging);
        setFrameDocument(
          `${DOCX_FRAME_PREFIX}${staging.innerHTML}${DOCX_FRAME_SUFFIX}`,
        );
      })
      .catch(() => {
        if (!cancelled) setErrorType("parse");
      })
      .finally(() => {
        stagingHost.remove();
      });

    return () => {
      cancelled = true;
      stagingHost.remove();
    };
  }, [fileDownload.data]);

  useEffect(() => {
    if (fileDownload.error) setErrorType("load");
  }, [fileDownload.error]);

  if (errorType) {
    return (
      <div className={cn("relative overflow-auto", className)} {...restProps}>
        <div className="text-muted-foreground flex h-64 items-center justify-center">
          <p>
            {errorType === "parse"
              ? "This document could not be read."
              : "This document could not be loaded."}
          </p>
        </div>
      </div>
    );
  }

  const isLoading = fileDownload.isLoading || !isReady;

  return (
    <div
      className={cn("relative min-h-0 overflow-hidden", className)}
      {...restProps}
    >
      {isLoading && (
        <div className="bg-background/80 absolute inset-0 z-10 flex items-center justify-center">
          {fileDownload.progress ? (
            <FileDownloadProgressIndicator progress={fileDownload.progress} />
          ) : (
            <div className="text-muted-foreground flex flex-col items-center gap-2">
              <Loader2Icon className="size-6 animate-spin" />
              <p>
                {fileDownload.isLoading
                  ? "Loading document…"
                  : "Reading document…"}
              </p>
            </div>
          )}
        </div>
      )}
      {frameDocument && (
        <iframe
          title="Document preview"
          sandbox=""
          referrerPolicy="no-referrer"
          srcDoc={frameDocument}
          className="h-full min-h-64 w-full border-0"
          onLoad={() => setIsReady(true)}
        />
      )}
    </div>
  );
}

/**
 * Defense in depth for any unexpected element emitted by docx-preview. Styles
 * remain intentionally intact, but are serialized safely and can only affect
 * the sandboxed document.
 */
function sanitizeRenderedDocument(container: HTMLElement): void {
  const externalHrefs = new WeakMap<Element, string>();

  container
    .querySelectorAll(
      "script, iframe, frame, frameset, object, embed, meta, base, link, form, input, button, textarea, select, option, title, noscript, template, xmp, noembed, plaintext",
    )
    .forEach((element) => element.remove());

  for (const element of container.querySelectorAll<HTMLElement>("*")) {
    for (const attribute of Array.from(element.attributes)) {
      const name = attribute.name.toLowerCase();
      if (
        name.startsWith("on") ||
        name === "data-tidebreak-external-href" ||
        name === "srcdoc" ||
        name === "srcset" ||
        name === "action" ||
        name === "formaction" ||
        name === "ping"
      ) {
        element.removeAttribute(attribute.name);
      }
    }

    if (element.hasAttribute("src")) {
      const src = element.getAttribute("src") ?? "";
      if (!src.startsWith("data:") && !src.startsWith("blob:")) {
        element.removeAttribute("src");
      }
    }

    for (const name of ["href", "xlink:href"]) {
      if (!element.hasAttribute(name)) continue;
      const href = element.getAttribute(name) ?? "";
      if (href.startsWith("#")) continue;
      const safeHref = safeExternalHref(href);
      if (
        safeHref &&
        name === "href" &&
        element.tagName.toLowerCase() === "a"
      ) {
        externalHrefs.set(element, safeHref);
      }
      element.removeAttribute(name);
    }
  }

  for (const anchor of container.querySelectorAll<HTMLAnchorElement>("a")) {
    const externalHref = externalHrefs.get(anchor);
    anchor.removeAttribute("target");
    anchor.removeAttribute("rel");
    if (externalHref) {
      anchor.title = `External link (copy this address): ${externalHref}`;
      if (!anchor.textContent?.includes(externalHref)) {
        anchor.append(document.createTextNode(` (${externalHref})`));
      }
    }
  }

  for (const page of container.querySelectorAll<HTMLElement>("section.docx")) {
    if (page.style.height) {
      page.style.setProperty("--tidebreak-docx-page-height", page.style.height);
    }
  }

  // <style> is an HTML raw-text element. Neutralize a DOCX-controlled closing
  // tag before innerHTML is embedded in srcdoc so it cannot become frame HTML.
  for (const style of container.querySelectorAll<HTMLStyleElement>("style")) {
    style.textContent = style.textContent?.replace(/<\/style/giu, "<\\/style") ?? "";
  }
}

function nextAnimationFrame(): Promise<void> {
  return new Promise((resolve) => {
    window.requestAnimationFrame(() => resolve());
  });
}

function waitForLayoutPass(): Promise<void> {
  return Promise.race([
    nextAnimationFrame(),
    new Promise<void>((resolve) => window.setTimeout(resolve, 100)),
  ]);
}

function safeExternalHref(href: string): string | null {
  try {
    const parsed = new URL(href);
    return parsed.protocol === "https:" && !parsed.username && !parsed.password
      ? parsed.href
      : null;
  } catch {
    return null;
  }
}
