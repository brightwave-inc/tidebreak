const DESKTOP_NOTIFICATIONS_KEY = "tidebreak.desktop-notifications";

/** Whether completion toasts and operating-system notifications are enabled. */
export function desktopNotificationsEnabled(): boolean {
  if (typeof window === "undefined") return true;
  return window.localStorage.getItem(DESKTOP_NOTIFICATIONS_KEY) !== "off";
}

export function setDesktopNotificationsEnabled(enabled: boolean): void {
  if (typeof window === "undefined") return;
  if (enabled) {
    window.localStorage.removeItem(DESKTOP_NOTIFICATIONS_KEY);
  } else {
    window.localStorage.setItem(DESKTOP_NOTIFICATIONS_KEY, "off");
  }
}
