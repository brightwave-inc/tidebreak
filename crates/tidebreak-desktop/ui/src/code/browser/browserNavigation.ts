export const DEFAULT_BROWSER_SEARCH_URL =
  "https://www.google.com/search?q=";
export const MAX_BROWSER_URL_CHARS = 8_192;

export type BrowserSecurity =
  | { kind: "secure"; label: "Secure" }
  | { kind: "local"; label: "Local" }
  | { kind: "insecure"; label: "Not secure" };

export type BrowserTarget =
  | { ok: true; url: string }
  | { ok: false; message: string };

/**
 * Turn one omnibox submission into a safe HTTP(S) target.
 *
 * Explicit schemes stay explicit. Loopback and host:port inputs default to
 * HTTP because that is how local dev servers normally listen. A hostname with
 * a dot defaults to HTTPS. Everything else becomes a search.
 */
export function browserTarget(
  input: string,
  searchUrl = DEFAULT_BROWSER_SEARCH_URL,
): BrowserTarget {
  const value = input.trim();
  if (!value) return { ok: false, message: "Enter an address or search" };

  const explicitScheme = /^[a-z][a-z\d+.-]*:(?!\d)/i.test(value);
  const noWhitespace = !/\s/.test(value);
  const localTarget = isLikelyLocalTarget(value);
  const webTarget = noWhitespace && isLikelyWebTarget(value);

  if (explicitScheme || localTarget || webTarget) {
    const candidate = localTarget
      ? `http://${value}`
      : explicitScheme
        ? value
        : `https://${value}`;
    return validateBrowserUrl(candidate);
  }

  return validateBrowserUrl(`${searchUrl}${encodeURIComponent(value)}`);
}

export function validateBrowserUrl(value: string): BrowserTarget {
  if (value.length > MAX_BROWSER_URL_CHARS) {
    return { ok: false, message: "That address is too long" };
  }
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return { ok: false, message: "That address is not valid" };
  }

  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return {
      ok: false,
      message: "Only HTTP and HTTPS addresses can open here",
    };
  }
  if (parsed.username || parsed.password) {
    return {
      ok: false,
      message: "Addresses with embedded credentials are not supported",
    };
  }
  if (!parsed.hostname) {
    return { ok: false, message: "That address has no host" };
  }
  if (parsed.href.length > MAX_BROWSER_URL_CHARS) {
    return { ok: false, message: "That address is too long" };
  }

  return { ok: true, url: parsed.href };
}

export function browserSecurity(url: string): BrowserSecurity {
  const parsed = new URL(url);
  if (parsed.protocol === "https:") return { kind: "secure", label: "Secure" };
  if (isLoopbackHost(parsed.hostname)) return { kind: "local", label: "Local" };
  return { kind: "insecure", label: "Not secure" };
}

export function browserDisplayAddress(url: string): string {
  const parsed = new URL(url);
  if (parsed.protocol === "http:") return parsed.href;
  const suffix = `${parsed.pathname}${parsed.search}${parsed.hash}`;
  return `${parsed.host}${suffix === "/" ? "" : suffix}`;
}

function isLikelyLocalTarget(value: string): boolean {
  const host = value.split(/[/?#]/, 1)[0]?.toLowerCase() ?? "";
  const hostname = normalizeHostname(host.startsWith("[")
    ? host.slice(0, host.indexOf("]") + 1)
    : host.split(":", 1)[0] ?? host);
  return (
    hostname === "localhost" ||
    hostname === "0.0.0.0" ||
    hostname === "127.0.0.1" ||
    hostname.startsWith("127.") ||
    hostname === "::1" ||
    isPrivateIpv4(hostname)
  );
}

function isLikelyWebTarget(value: string): boolean {
  const host = value.split(/[/?#]/, 1)[0] ?? "";
  return host.includes(".") || /^\[[0-9a-f:]+\](?::\d+)?$/i.test(host);
}

function isLoopbackHost(hostname: string): boolean {
  const host = normalizeHostname(hostname);
  return (
    host === "localhost" ||
    host.endsWith(".localhost") ||
    host === "0.0.0.0" ||
    host === "::1" ||
    host.startsWith("127.") ||
    isPrivateIpv4(host)
  );
}

function normalizeHostname(hostname: string): string {
  return hostname
    .toLowerCase()
    .replace(/^\[|\]$/g, "")
    .replace(/\.$/, "");
}

function isPrivateIpv4(hostname: string): boolean {
  const octets = hostname.split(".").map(Number);
  if (octets.length !== 4 || octets.some((part) => !Number.isInteger(part))) {
    return false;
  }
  return (
    octets[0] === 10 ||
    (octets[0] === 172 && (octets[1] ?? 0) >= 16 && (octets[1] ?? 0) <= 31) ||
    (octets[0] === 192 && octets[1] === 168)
  );
}
