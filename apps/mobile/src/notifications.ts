import { LocalNotifications } from "@capacitor/local-notifications";
import { Capacitor } from "@capacitor/core";

const LAST_NOTIFICATION_ID_KEY = "omgb:lastNotificationId";
let requested = false;

function nextNotificationId(): number {
  try {
    const raw = localStorage.getItem(LAST_NOTIFICATION_ID_KEY);
    let id = Number(raw) || 0;
    id = (id + 1) % 2_147_483_647;
    localStorage.setItem(LAST_NOTIFICATION_ID_KEY, String(id));
    return id;
  } catch {
    // localStorage may be unavailable; fall back to a time-based id.
    return Math.floor(Date.now() / 1000) % 2_147_483_647;
  }
}

export async function requestNotificationPermission(): Promise<void> {
  if (requested) return;
  requested = true;

  if (Capacitor.isNativePlatform()) {
    try {
      const result = await LocalNotifications.requestPermissions();
      if (result.display !== "granted") return;
    } catch {
      // fall through to web permission
    }
  }

  if ("Notification" in window && Notification.permission === "default") {
    try {
      await Notification.requestPermission();
    } catch {
      // ignore
    }
  }
}

export async function notifyCompletion(title: string, body?: string): Promise<void> {
  if (typeof document !== "undefined" && !document.hidden) return;

  if (Capacitor.isNativePlatform()) {
    try {
      const perms = await LocalNotifications.checkPermissions();
      if (perms.display === "granted") {
        await LocalNotifications.schedule({
          notifications: [
            {
              title,
              body: body ?? "",
              id: nextNotificationId(),
              schedule: { at: new Date(Date.now() + 1000) },
            },
          ],
        });
        return;
      }
    } catch {
      // fall through to web notification
    }
  }

  if ("Notification" in window && Notification.permission === "granted") {
    try {
      new Notification(title, { body });
    } catch {
      // ignore
    }
  }
}
