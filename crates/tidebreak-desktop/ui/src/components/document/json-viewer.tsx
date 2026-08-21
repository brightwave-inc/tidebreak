import {
  ChevronsDownUp,
  ChevronDown,
  ChevronRight,
  ChevronRightIcon,
  Loader2Icon,
} from "lucide-react";
import {
  createContext,
  type HTMLAttributes,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  useFileDownload,
  type FileBytesSource,
} from "@/document/useFileDownload";
import { cn } from "@/lib/utils";
import { FileDownloadProgressIndicator } from "./FileDownloadProgress";

// ---------------------------------------------------------------------------
// Collapse signaling
// ---------------------------------------------------------------------------

type CollapseSignal = { counter: number };

const CollapseSignalContext = createContext<CollapseSignal>({ counter: 0 });

// ---------------------------------------------------------------------------
// Navigate-to-path signaling — expands + scrolls to a specific path
// ---------------------------------------------------------------------------

type NavigateSignal = { counter: number; path: string };

const NavigateSignalContext = createContext<NavigateSignal>({
  counter: 0,
  path: "",
});

// ---------------------------------------------------------------------------
// Highlight signaling — avoids threading highlightPath/ref through every node
// ---------------------------------------------------------------------------

const HighlightSignalContext = createContext<string | undefined>(undefined);

type HighlightRefSetter = (el: HTMLDivElement | null) => void;
const HighlightRefSetterContext = createContext<HighlightRefSetter>(() => {});

// ---------------------------------------------------------------------------
// JSON traversal helpers
// ---------------------------------------------------------------------------

/** Resolve a value at a dot-notation path within parsed JSON. */
function resolveJsonPath(root: unknown, path: string): unknown {
  if (!path) return root;
  const segments = path.split(".");
  let current = root;
  for (const seg of segments) {
    if (current === null || current === undefined) return undefined;
    if (Array.isArray(current)) {
      const idx = Number(seg);
      if (Number.isNaN(idx)) return undefined;
      current = current[idx];
    } else if (typeof current === "object") {
      current = (current as Record<string, unknown>)[seg];
    } else {
      return undefined;
    }
  }
  return current;
}

/** Get the child keys/indices of a value (object keys or array indices). */
function getChildKeys(value: unknown): string[] {
  if (value !== null && typeof value === "object" && !Array.isArray(value)) {
    return Object.keys(value);
  }
  if (Array.isArray(value)) {
    return value.map((_, i) => String(i));
  }
  return [];
}

/** Get a display label for a child value. */
function getChildLabel(key: string): string {
  return key;
}

/** Get the type indicator for a value. */
function getTypeIndicator(value: unknown): string {
  if (Array.isArray(value)) return `[ ] `;
  if (value !== null && typeof value === "object") return `{ } `;
  return "";
}

function isContainer(value: unknown): boolean {
  return value !== null && typeof value === "object";
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

interface JsonViewerProps extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
  /** Glom-style dot-notation path to highlight, e.g. "items.0.invoice_number" */
  highlightPath?: string;
}

export function JsonViewer({
  source,
  highlightPath,
  className,
  ...props
}: JsonViewerProps) {
  const fileDownload = useFileDownload(source, {
    parseAs: "text",
  });

  const parsed = useMemo(() => {
    if (!fileDownload.data) return undefined;
    try {
      return JSON.parse(fileDownload.data) as unknown;
    } catch {
      return undefined;
    }
  }, [fileDownload.data]);

  const highlightRef = useRef<HTMLDivElement>(null);
  const setHighlightEl = useCallback((el: HTMLDivElement | null) => {
    highlightRef.current = el;
  }, []);
  const nodeRefs = useRef<Map<string, HTMLDivElement>>(new Map());

  const [collapseSignal, setCollapseSignal] = useState<CollapseSignal>({
    counter: 0,
  });

  const [navigateSignal, setNavigateSignal] = useState<NavigateSignal>({
    counter: 0,
    path: "",
  });

  const collapseAll = useCallback(
    () => setCollapseSignal((s) => ({ counter: s.counter + 1 })),
    [],
  );

  const navigateToPath = useCallback((path: string) => {
    setNavigateSignal((s) => ({ counter: s.counter + 1, path }));
    // Retry scroll until the target node has expanded and registered its ref
    const tryScroll = (attempts: number) => {
      const el = nodeRefs.current.get(path);
      if (el) {
        el.scrollIntoView({ behavior: "smooth", block: "start" });
      } else if (attempts < 10) {
        requestAnimationFrame(() => tryScroll(attempts + 1));
      }
    };
    requestAnimationFrame(() => tryScroll(0));
  }, []);

  const registerNodeRef = useCallback(
    (path: string, el: HTMLDivElement | null) => {
      if (el) {
        nodeRefs.current.set(path, el);
      } else {
        nodeRefs.current.delete(path);
      }
    },
    [],
  );

  useEffect(() => {
    if (highlightRef.current) {
      highlightRef.current.scrollIntoView({
        behavior: "smooth",
        block: "center",
      });
    }
  }, [highlightPath, parsed]);

  if (fileDownload.error) {
    return (
      <div className={cn("relative overflow-auto", className)} {...props}>
        <div className="text-muted-foreground flex h-64 items-center justify-center">
          <p>Failed to download document</p>
        </div>
      </div>
    );
  }

  if (fileDownload.isLoading) {
    return (
      <div
        className={cn("flex items-center justify-center", className)}
        {...props}
      >
        {fileDownload.progress ? (
          <FileDownloadProgressIndicator progress={fileDownload.progress} />
        ) : (
          <Loader2Icon className="text-muted-foreground size-6 animate-spin" />
        )}
      </div>
    );
  }

  if (parsed === undefined) {
    return (
      <div className={cn("relative overflow-auto", className)} {...props}>
        <div className="text-muted-foreground flex h-64 items-center justify-center">
          <p>Unable to parse JSON</p>
        </div>
      </div>
    );
  }

  return (
    <div
      className={cn("flex grow flex-col overflow-hidden", className)}
      {...props}
    >
      <JsonBreadcrumbBar
        data={parsed}
        onCollapseAll={collapseAll}
        onNavigate={navigateToPath}
      />
      <div className="bg-page-background grow overflow-auto p-6 font-mono text-sm">
        <HighlightSignalContext.Provider value={highlightPath}>
          <HighlightRefSetterContext.Provider value={setHighlightEl}>
            <CollapseSignalContext.Provider value={collapseSignal}>
              <NavigateSignalContext.Provider value={navigateSignal}>
                <JsonNode
                  value={parsed}
                  path=""
                  registerNodeRef={registerNodeRef}
                  depth={0}
                />
              </NavigateSignalContext.Provider>
            </CollapseSignalContext.Provider>
          </HighlightRefSetterContext.Provider>
        </HighlightSignalContext.Provider>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Breadcrumb navigation bar
// ---------------------------------------------------------------------------

function JsonBreadcrumbBar({
  data,
  onCollapseAll,
  onNavigate,
}: {
  data: unknown;
  onCollapseAll: () => void;
  onNavigate: (path: string) => void;
}) {
  const [breadcrumb, setBreadcrumb] = useState<string[]>([]);

  // Reset breadcrumb when the underlying JSON data changes
  useEffect(() => setBreadcrumb([]), [data]);

  const handleSelect = useCallback(
    (depth: number, key: string) => {
      const newBreadcrumb = [...breadcrumb.slice(0, depth), key];
      setBreadcrumb(newBreadcrumb);
      onNavigate(newBreadcrumb.join("."));
    },
    [breadcrumb, onNavigate],
  );

  // Build breadcrumb segments: root + each level the user has navigated into
  const segments = useMemo(() => {
    const result: {
      parentPath: string;
      parentValue: unknown;
      selectedKey: string | null;
    }[] = [];

    // Root level
    result.push({
      parentPath: "",
      parentValue: data,
      selectedKey: breadcrumb[0] ?? null,
    });

    // Each navigated level
    let currentPath = "";
    for (let i = 0; i < breadcrumb.length; i++) {
      const seg = breadcrumb[i];
      if (seg === undefined) break;
      currentPath = currentPath ? `${currentPath}.${seg}` : seg;
      const value = resolveJsonPath(data, currentPath);
      if (!isContainer(value)) break;
      result.push({
        parentPath: currentPath,
        parentValue: value,
        selectedKey: breadcrumb[i + 1] ?? null,
      });
    }

    return result;
  }, [data, breadcrumb]);

  return (
    <div className="bg-muted/50 flex items-center gap-0.5 border-b px-2 py-1">
      <Button
        variant="ghost"
        size="icon-xs"
        onClick={onCollapseAll}
        title="Collapse all"
      >
        <ChevronsDownUp className="size-3.5" />
        <span className="sr-only">Collapse all</span>
      </Button>
      <div className="bg-border mx-1 h-4 w-px" />
      <div className="flex min-w-0 items-center gap-0.5 overflow-x-auto">
        {segments.map((seg, i) => {
          const childKeys = getChildKeys(seg.parentValue);
          if (childKeys.length === 0) return null;
          const parentIsArray = Array.isArray(seg.parentValue);
          return (
            <BreadcrumbSegment
              key={seg.parentPath || "__root"}
              depth={i}
              childKeys={childKeys}
              parentValue={seg.parentValue}
              parentIsArray={parentIsArray}
              selectedKey={seg.selectedKey}
              onSelect={handleSelect}
              showSeparator={i > 0}
            />
          );
        })}
      </div>
    </div>
  );
}

function BreadcrumbSegment({
  depth,
  childKeys,
  parentValue,
  parentIsArray,
  selectedKey,
  onSelect,
  showSeparator,
}: {
  depth: number;
  childKeys: string[];
  parentValue: unknown;
  parentIsArray: boolean;
  selectedKey: string | null;
  onSelect: (depth: number, key: string) => void;
  showSeparator: boolean;
}) {
  const [open, setOpen] = useState(false);

  const label = selectedKey
    ? parentIsArray
      ? `[${selectedKey}]`
      : selectedKey
    : parentIsArray
      ? "[ ]"
      : "{ }";

  return (
    <>
      {showSeparator && (
        <ChevronRightIcon className="text-muted-foreground size-3 shrink-0" />
      )}
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <button
            className={cn(
              "text-muted-foreground hover:text-foreground hover:bg-muted shrink-0 rounded px-1.5 py-0.5 font-mono text-xs transition-colors",
              selectedKey && "text-foreground",
            )}
          >
            {label}
          </button>
        </PopoverTrigger>
        <PopoverContent
          align="start"
          className="max-h-64 w-48 overflow-y-auto p-1"
        >
          {childKeys.map((key) => {
            const childValue = Array.isArray(parentValue)
              ? parentValue[Number(key)]
              : (parentValue as Record<string, unknown>)[key];
            const typeIndicator = getTypeIndicator(childValue);
            const displayLabel = getChildLabel(key);
            return (
              <button
                key={key}
                onClick={() => {
                  onSelect(depth, key);
                  setOpen(false);
                }}
                className={cn(
                  "hover:bg-muted flex w-full items-center gap-1 rounded px-2 py-1 text-left font-mono text-xs",
                  key === selectedKey && "bg-muted",
                )}
              >
                <span className="text-muted-foreground">{typeIndicator}</span>
                <span className="truncate">{displayLabel}</span>
              </button>
            );
          })}
        </PopoverContent>
      </Popover>
    </>
  );
}

// ---------------------------------------------------------------------------
// Highlight helpers
// ---------------------------------------------------------------------------

function isOnHighlightPath(
  currentPath: string,
  highlightPath: string,
): boolean {
  if (!highlightPath) return false;
  if (currentPath === highlightPath) return true;
  return highlightPath.startsWith(currentPath + ".");
}

function isExactHighlightPath(
  currentPath: string,
  highlightPath: string,
): boolean {
  return !!highlightPath && currentPath === highlightPath;
}

function isOnNavigatePath(currentPath: string, navigatePath: string): boolean {
  if (!navigatePath) return false;
  if (currentPath === navigatePath) return true;
  return navigatePath.startsWith(currentPath + ".");
}

// ---------------------------------------------------------------------------
// Recursive renderer
// ---------------------------------------------------------------------------

interface JsonNodeProps {
  value: unknown;
  path: string;
  registerNodeRef?: (path: string, el: HTMLDivElement | null) => void;
  depth: number;
  fieldName?: string;
  isLast?: boolean;
}

function JsonNode({
  value,
  path,
  registerNodeRef,
  depth,
  fieldName,
  isLast = true,
}: JsonNodeProps) {
  const collapseSignal = useContext(CollapseSignalContext);
  const navigateSignal = useContext(NavigateSignalContext);
  const highlightPath = useContext(HighlightSignalContext);

  const isHighlighted = highlightPath
    ? isExactHighlightPath(path, highlightPath)
    : false;
  const isAncestor = highlightPath
    ? isOnHighlightPath(path, highlightPath) && !isHighlighted
    : false;

  const [expanded, setExpanded] = useState(
    depth < 1 || isAncestor || isHighlighted,
  );

  // Respond to collapse-all signal (keep root expanded)
  useEffect(() => {
    if (collapseSignal.counter === 0) return;
    setExpanded(depth < 1);
  }, [collapseSignal, depth]);

  // Respond to navigate signal — expand nodes on the navigate path
  useEffect(() => {
    if (navigateSignal.counter === 0 || !path) return;
    if (isOnNavigatePath(path, navigateSignal.path)) {
      setExpanded(true);
    }
  }, [navigateSignal, path]);

  // When highlight path changes, expand ancestors
  useEffect(() => {
    if (isAncestor || isHighlighted) {
      setExpanded(true);
    }
  }, [isAncestor, isHighlighted]);

  const toggle = useCallback(() => setExpanded((e) => !e), []);

  // Register this node's ref for scroll-to navigation
  const nodeRefCallback = useCallback(
    (el: HTMLDivElement | null) => {
      if (registerNodeRef && path) {
        registerNodeRef(path, el);
      }
    },
    [registerNodeRef, path],
  );

  const comma = isLast ? "" : ",";

  if (value === null) {
    return (
      <Line
        fieldName={fieldName}
        isHighlighted={isHighlighted}
        navRef={nodeRefCallback}
      >
        <span className="text-orange-600 dark:text-orange-400">null</span>
        {comma}
      </Line>
    );
  }

  if (typeof value === "boolean") {
    return (
      <Line
        fieldName={fieldName}
        isHighlighted={isHighlighted}
        navRef={nodeRefCallback}
      >
        <span className="text-orange-600 dark:text-orange-400">
          {value ? "true" : "false"}
        </span>
        {comma}
      </Line>
    );
  }

  if (typeof value === "number") {
    return (
      <Line
        fieldName={fieldName}
        isHighlighted={isHighlighted}
        navRef={nodeRefCallback}
      >
        <span className="text-blue-600 dark:text-blue-400">
          {String(value)}
        </span>
        {comma}
      </Line>
    );
  }

  if (typeof value === "string") {
    return (
      <Line
        fieldName={fieldName}
        isHighlighted={isHighlighted}
        navRef={nodeRefCallback}
      >
        <StringValue value={value} />
        {comma}
      </Line>
    );
  }

  if (Array.isArray(value)) {
    return (
      <CollapsibleNode
        fieldName={fieldName}
        openBracket="["
        closeBracket="]"
        count={value.length}
        expanded={expanded}
        onToggle={toggle}
        comma={comma}
        isHighlighted={isHighlighted}
        navRef={nodeRefCallback}
      >
        {value.map((item, i) => (
          <JsonNode
            key={i}
            value={item}
            path={path ? `${path}.${i}` : String(i)}
            registerNodeRef={registerNodeRef}
            depth={depth + 1}
            isLast={i === value.length - 1}
          />
        ))}
      </CollapsibleNode>
    );
  }

  if (typeof value === "object") {
    const entries = Object.entries(value);
    return (
      <CollapsibleNode
        fieldName={fieldName}
        openBracket="{"
        closeBracket="}"
        count={entries.length}
        expanded={expanded}
        onToggle={toggle}
        comma={comma}
        isHighlighted={isHighlighted}
        navRef={nodeRefCallback}
      >
        {entries.map(([key, val], i) => (
          <JsonNode
            key={key}
            value={val}
            path={path ? `${path}.${key}` : key}
            registerNodeRef={registerNodeRef}
            depth={depth + 1}
            fieldName={key}
            isLast={i === entries.length - 1}
          />
        ))}
      </CollapsibleNode>
    );
  }

  return (
    <Line fieldName={fieldName} isHighlighted={false} navRef={nodeRefCallback}>
      <span>{String(value)}</span>
      {comma}
    </Line>
  );
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

function Line({
  fieldName,
  children,
  isHighlighted,
  navRef,
}: {
  fieldName?: string;
  children: ReactNode;
  isHighlighted: boolean;
  navRef?: (el: HTMLDivElement | null) => void;
}) {
  const setHighlightEl = useContext(HighlightRefSetterContext);

  const refCallback = useCallback(
    (el: HTMLDivElement | null) => {
      if (isHighlighted) {
        setHighlightEl(el);
      }
      navRef?.(el);
    },
    [isHighlighted, setHighlightEl, navRef],
  );

  return (
    <div
      ref={refCallback}
      className={cn(
        "flex flex-wrap gap-x-1 py-px pl-6",
        isHighlighted && "rounded bg-yellow-200/40 dark:bg-yellow-500/20",
      )}
    >
      {fieldName !== undefined && (
        <>
          <span className="text-violet-700 dark:text-violet-400">
            &quot;{fieldName}&quot;
          </span>
          <span className="text-foreground/60">:</span>
        </>
      )}
      {children}
    </div>
  );
}

function StringValue({ value }: { value: string }) {
  const MAX_INLINE_LENGTH = 300;
  const truncated =
    value.length > MAX_INLINE_LENGTH
      ? value.slice(0, MAX_INLINE_LENGTH) + "…"
      : value;
  return (
    <span className="text-green-700 dark:text-green-400">
      &quot;{truncated}&quot;
    </span>
  );
}

function CollapsibleNode({
  fieldName,
  openBracket,
  closeBracket,
  count,
  expanded,
  onToggle,
  comma,
  children,
  isHighlighted,
  navRef,
}: {
  fieldName?: string;
  openBracket: string;
  closeBracket: string;
  count: number;
  expanded: boolean;
  onToggle: () => void;
  comma: string;
  children: ReactNode;
  isHighlighted: boolean;
  navRef?: (el: HTMLDivElement | null) => void;
}) {
  const setHighlightEl = useContext(HighlightRefSetterContext);

  const refCallback = useCallback(
    (el: HTMLDivElement | null) => {
      if (isHighlighted) {
        setHighlightEl(el);
      }
      navRef?.(el);
    },
    [isHighlighted, setHighlightEl, navRef],
  );

  return (
    <div>
      <div
        ref={refCallback}
        className={cn(
          "hover:bg-muted/50 flex cursor-pointer items-center gap-x-1 py-px pl-1 select-none",
          isHighlighted && "rounded bg-yellow-200/40 dark:bg-yellow-500/20",
        )}
        onClick={onToggle}
      >
        {expanded ? (
          <ChevronDown className="text-muted-foreground size-4 shrink-0" />
        ) : (
          <ChevronRight className="text-muted-foreground size-4 shrink-0" />
        )}
        {fieldName !== undefined && (
          <>
            <span className="text-violet-700 dark:text-violet-400">
              &quot;{fieldName}&quot;
            </span>
            <span className="text-foreground/60">:</span>
          </>
        )}
        <span className="text-foreground/60">{openBracket}</span>
        {!expanded && (
          <>
            <span className="text-muted-foreground text-xs">
              {count} {count === 1 ? "item" : "items"}
            </span>
            <span className="text-foreground/60">
              {closeBracket}
              {comma}
            </span>
          </>
        )}
      </div>
      {expanded && (
        <>
          <div className="border-border/50 ml-3 border-l">{children}</div>
          <div className="text-foreground/60 py-px pl-4">
            {closeBracket}
            {comma}
          </div>
        </>
      )}
    </div>
  );
}

export default JsonViewer;
