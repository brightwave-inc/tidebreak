export const REASON_URL_INVALID = "url_invalid";
export const REASON_REQUIRES_TLS = "requires_tls";

export class UrlValidationError extends Error {
  readonly reason: string;

  constructor(reason: string, message?: string) {
    super(message ?? reason);
    this.name = "UrlValidationError";
    this.reason = reason;
  }
}

function hostIsLoopback(host: string): boolean {
  if (host.toLowerCase() === "localhost") {
    return true;
  }
  const bare = host.replace(/^\[/, "").replace(/]$/, "");
  return bare === "127.0.0.1" || bare === "::1" || bare === "0:0:0:0:0:0:0:1";
}

/** Normalize a user-entered base URL, matching desktop `validated_base_url`. */
export function validatedBaseUrl(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed.length === 0) {
    throw new UrlValidationError(REASON_URL_INVALID);
  }
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    throw new UrlValidationError(REASON_URL_INVALID);
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new UrlValidationError(REASON_URL_INVALID);
  }
  if (!url.hostname) {
    throw new UrlValidationError(REASON_URL_INVALID);
  }
  if (url.username || url.password) {
    throw new UrlValidationError(REASON_URL_INVALID);
  }
  if (url.search || url.hash) {
    throw new UrlValidationError(REASON_URL_INVALID);
  }
  if (url.protocol === "http:" && !hostIsLoopback(url.hostname)) {
    throw new UrlValidationError(REASON_REQUIRES_TLS);
  }
  return url.toString().replace(/\/+$/, "");
}

export function urlsMatch(left: string, right: string): boolean {
  return validatedBaseUrl(left) === validatedBaseUrl(right);
}
