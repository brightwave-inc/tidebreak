export const recoveryStorageKey = "tidebreak.browser-fixture.recovery";
export const recoveryCookieName = "tidebreak_fixture_recovery";

function checkedMarker(value) {
  if (typeof value !== "string" || !/^[A-Za-z0-9_-]{1,64}$/.test(value)) {
    throw new Error(
      "Use 1–64 letters, numbers, underscores, or hyphens for the recovery marker.",
    );
  }
  return value;
}

export function readRecoveryMarkers(storage, cookies) {
  const localValue = storage.getItem(recoveryStorageKey);
  const prefix = recoveryCookieName + "=";
  const matches = String(cookies)
    .split(";")
    .map((part) => part.trim())
    .filter((part) => part.startsWith(prefix));
  if (matches.length > 1) throw new Error("The recovery cookie is ambiguous.");
  let cookieValue = null;
  if (matches.length) {
    try {
      cookieValue = checkedMarker(
        decodeURIComponent(matches[0].slice(prefix.length)),
      );
    } catch {
      throw new Error("The recovery cookie is invalid.");
    }
  }
  return {
    localStorage: localValue === null ? null : checkedMarker(localValue),
    cookie: cookieValue,
  };
}

export function saveRecoveryMarker(storage, doc, value) {
  const marker = checkedMarker(value);
  storage.setItem(recoveryStorageKey, marker);
  doc.cookie =
    recoveryCookieName +
    "=" +
    encodeURIComponent(marker) +
    "; Max-Age=604800; Path=/; SameSite=Lax";
  return readRecoveryMarkers(storage, doc.cookie);
}

export function mountRecoveryPage(doc, storage) {
  const input = doc.querySelector("#fixture-marker");
  const localOutput = doc.querySelector("#local-storage-marker");
  const cookieOutput = doc.querySelector("#cookie-marker");
  const status = doc.querySelector("#recovery-status");
  const download = doc.querySelector("#slow-download");
  const render = (markers) => {
    localOutput.textContent =
      "Local storage marker: " + (markers.localStorage ?? "Missing");
    cookieOutput.textContent =
      "Cookie marker: " + (markers.cookie ?? "Missing");
  };
  const updateDownload = () => {
    try {
      download.setAttribute(
        "href",
        "/slow-download?token=" +
          encodeURIComponent(checkedMarker(input.value)),
      );
      download.removeAttribute("aria-disabled");
    } catch {
      download.removeAttribute("href");
      download.setAttribute("aria-disabled", "true");
    }
  };
  const read = () => {
    try {
      const markers = readRecoveryMarkers(storage, doc.cookie);
      render(markers);
      status.textContent = "Recovery markers read.";
      return markers;
    } catch {
      localOutput.textContent = "Local storage marker: Unavailable";
      cookieOutput.textContent = "Cookie marker: Unavailable";
      status.textContent = "Could not read recovery markers.";
      return null;
    }
  };
  doc.querySelector("#save-recovery-marker").addEventListener("click", () => {
    try {
      const markers = saveRecoveryMarker(storage, doc, input.value);
      render(markers);
      status.textContent =
        markers.localStorage === input.value && markers.cookie === input.value
          ? "Recovery marker saved."
          : "The browser did not retain both recovery markers.";
    } catch {
      status.textContent =
        "Could not save the recovery marker. Use 1–64 letters, numbers, underscores, or hyphens.";
    }
    updateDownload();
  });
  doc.querySelector("#read-recovery-markers").addEventListener("click", read);
  input.addEventListener("input", updateDownload);
  const markers = read();
  if (markers?.localStorage && markers.localStorage === markers.cookie) {
    input.value = markers.localStorage;
  }
  updateDownload();
}

if (typeof document !== "undefined") {
  mountRecoveryPage(document, globalThis.localStorage);
}
