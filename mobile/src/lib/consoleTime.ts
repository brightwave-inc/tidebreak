/**
 * Clocks, as the console screens read them.
 *
 * Tidewatch reached for `date-fns` here. These four functions are its whole
 * use of it on the member screens, and each is a few lines of arithmetic, so
 * they are written out rather than adding a dependency to carry them.
 *
 * Every clock the console shows is relative. A sandbox's absolute start time
 * is rarely the question — "is this still moving" is — and a relative reading
 * survives the timezone gap between the phone and the installation without
 * either having to be named.
 */

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** "3 minutes ago" for an ISO timestamp, or the raw text when it is not one. */
export function relative(iso: string, now: number = Date.now()): string {
  const at = new Date(iso).getTime();
  if (Number.isNaN(at)) {
    return iso;
  }
  const delta = now - at;
  const ahead = delta < 0;
  const magnitude = Math.abs(delta);
  const phrase = magnitudePhrase(magnitude);
  if (phrase === null) {
    return "just now";
  }
  return ahead ? `in ${phrase}` : `${phrase} ago`;
}

function magnitudePhrase(ms: number): string | null {
  if (ms < 45_000) {
    return null;
  }
  if (ms < HOUR) {
    return plural(Math.round(ms / MINUTE), "minute");
  }
  if (ms < DAY) {
    return plural(Math.round(ms / HOUR), "hour");
  }
  if (ms < 30 * DAY) {
    return plural(Math.round(ms / DAY), "day");
  }
  if (ms < 365 * DAY) {
    return plural(Math.round(ms / (30 * DAY)), "month");
  }
  return plural(Math.round(ms / (365 * DAY)), "year");
}

function plural(count: number, unit: string): string {
  return `${count} ${unit}${count === 1 ? "" : "s"}`;
}

/**
 * A configured bound in seconds, read the way a person would say it.
 *
 * These are ceilings out of a sandbox profile, not elapsed clocks, so they
 * never want second resolution — and "5400 seconds" makes the reader do the
 * division before they can tell whether the bound is generous.
 */
export function duration(seconds: number): string {
  if (seconds < 60) {
    return `${seconds}s`;
  }
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) {
    return `${minutes}m`;
  }
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours}h` : `${hours}h ${rest}m`;
}

const MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
] as const;

function monthName(index: number): string {
  return MONTHS[index] ?? "";
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

/**
 * The heading a timeline puts above the first event of each day. Named days
 * near the present, dated ones further back, and the year only once it stops
 * being this one — a long-running sandbox's earliest events are otherwise
 * indistinguishable from the same date a year ago.
 */
export function dayLabel(iso: string, now: number = Date.now()): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) {
    return iso;
  }
  const today = new Date(now);
  if (sameDay(at, today)) {
    return "Today";
  }
  const yesterday = new Date(now - DAY);
  if (sameDay(at, yesterday)) {
    return "Yesterday";
  }
  const stamp = `${monthName(at.getMonth())} ${at.getDate()}`;
  return at.getFullYear() === today.getFullYear()
    ? stamp
    : `${stamp}, ${at.getFullYear()}`;
}

function sameDay(left: Date, right: Date): boolean {
  return (
    left.getFullYear() === right.getFullYear() &&
    left.getMonth() === right.getMonth() &&
    left.getDate() === right.getDate()
  );
}

/** "Mar 4, 09:15" — the one place the console shows an absolute moment. */
export function stamp(iso: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) {
    return iso;
  }
  return `${monthName(at.getMonth())} ${at.getDate()}, ${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/** The same, for the unix-seconds timestamps the subscription views carry. */
export function stampUnixSeconds(seconds: number): string {
  const at = new Date(seconds * 1000);
  return Number.isNaN(at.getTime()) ? "—" : stamp(at.toISOString());
}

/** "09:15" — a timeline row's own time, under its day heading. */
export function clockTime(iso: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) {
    return "";
  }
  return `${pad(at.getHours())}:${pad(at.getMinutes())}`;
}
