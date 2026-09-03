import { Loader2 } from "lucide-react";

import type { SpecDiscoveryInfo } from "../api";
import { Button } from "@/components/ui/button";

/** A copyable OpenAPI 3 JSON document with one GET, a path parameter, and a
 * bearer header — enough to ingest when the vendor publishes nothing. */
export const MINIMAL_OPENAPI_EXAMPLE = `{
  "openapi": "3.0.3",
  "info": { "title": "Example", "version": "1" },
  "paths": {
    "/items/{id}": {
      "get": {
        "operationId": "getItem",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "security": [{ "bearer": [] }]
      }
    }
  },
  "components": {
    "securitySchemes": {
      "bearer": { "type": "http", "scheme": "bearer" }
    }
  }
}`;

export function ingestErrorGuidance(message: string): string {
  const lower = message.toLowerCase();
  if (lower.includes("scheme must be https")) {
    return `${message} Use an https URL.`;
  }
  if (lower.includes("html")) {
    return `${message} That URL is a documentation page. Find a link to the OpenAPI JSON, or paste the JSON itself.`;
  }
  if (lower.includes("specification index")) {
    return message;
  }
  if (lower.includes("yaml")) {
    return `${message} Convert it to JSON, or point at a .json URL.`;
  }
  if (lower.includes("swagger 2.0")) {
    return `${message} Export or convert it to OpenAPI 3 JSON.`;
  }
  return message;
}

export function NoPublicDocumentGuidance({
  onPasteExample,
}: {
  onPasteExample: () => void;
}) {
  return (
    <details className="text-sm text-muted-foreground">
      <summary className="cursor-pointer font-medium text-foreground">
        No public OpenAPI document?
      </summary>
      <ol className="mt-2 flex list-decimal flex-col gap-2 pl-5">
        <li>
          Check the vendor&apos;s developer portal for an OpenAPI, Swagger, or
          API reference download link, then paste that URL.
        </li>
        <li>
          If they publish no document, author a minimal OpenAPI 3 JSON listing
          only the operations you need.
          <div className="mt-1.5 flex flex-col gap-1.5">
            <pre className="overflow-x-auto rounded-md border bg-transparent p-2 font-mono text-xs text-foreground">
              {MINIMAL_OPENAPI_EXAMPLE}
            </pre>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="self-start"
              onClick={onPasteExample}
            >
              Paste this example
            </Button>
          </div>
        </li>
        <li>
          If they offer an MCP server instead, add it under Settings → MCP
          servers on this page.
        </li>
      </ol>
    </details>
  );
}

export function DiscoveryResults({
  discovery,
  discovering,
  onChoose,
}: {
  discovery: SpecDiscoveryInfo | null;
  discovering: boolean;
  onChoose: (url: string) => void;
}) {
  if (discovering) {
    return (
      <p className="flex items-center gap-2 text-sm text-muted-foreground">
        <Loader2 size={14} className="animate-spin" />
        Searching well-known OpenAPI locations…
      </p>
    );
  }
  if (discovery === null) return null;
  const usable = discovery.candidates.filter(
    (candidate) => candidate.operation_count != null,
  );
  const indexes = discovery.candidates.filter(
    (candidate) =>
      candidate.child_urls != null && candidate.child_urls.length > 0,
  );
  const unsupported = discovery.candidates.filter(
    (candidate) =>
      candidate.unsupported_reason != null &&
      (candidate.child_urls == null || candidate.child_urls.length === 0),
  );
  if (discovery.candidates.length === 0) {
    return (
      <div className="flex flex-col gap-2 text-sm text-muted-foreground">
        <p>No OpenAPI document turned up at the usual locations.</p>
        <TriedList tried={discovery.tried} />
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      {usable.length > 0 && (
        <ul className="flex flex-col gap-1" aria-label="OpenAPI candidates">
          {usable.map((candidate) => (
            <li key={candidate.url} className="flex items-center gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => onChoose(candidate.url)}
              >
                Use this document
              </Button>
              <span className="font-mono text-xs">
                {candidate.url}
                {candidate.operation_count != null
                  ? ` · ${candidate.operation_count} operation${candidate.operation_count === 1 ? "" : "s"}`
                  : ""}
              </span>
            </li>
          ))}
        </ul>
      )}
      {indexes.map((candidate) => (
        <div key={candidate.url} className="flex flex-col gap-1.5">
          <p className="text-sm text-muted-foreground">
            <span className="font-mono text-xs">{candidate.url}</span> is a spec
            index. Pick a document:
          </p>
          <ul className="flex flex-col gap-1" aria-label="Child documents">
            {candidate.child_urls?.map((childUrl) => (
              <li key={childUrl} className="flex items-center gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => onChoose(childUrl)}
                >
                  Use this document
                </Button>
                <span className="font-mono text-xs">{childUrl}</span>
              </li>
            ))}
          </ul>
        </div>
      ))}
      {unsupported.map((candidate) => (
        <p key={candidate.url} className="text-sm text-muted-foreground">
          <span className="font-mono text-xs">{candidate.url}</span>
          {": "}
          {ingestErrorGuidance(candidate.unsupported_reason ?? "")}
        </p>
      ))}
      {usable.length === 0 && <TriedList tried={discovery.tried} />}
    </div>
  );
}

function TriedList({ tried }: { tried: string[] }) {
  return (
    <details>
      <summary className="cursor-pointer text-sm">Locations tried</summary>
      <ul className="mt-1 font-mono text-xs">
        {tried.map((url) => (
          <li key={url}>{url}</li>
        ))}
      </ul>
    </details>
  );
}
