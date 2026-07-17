import { LocalNotifications } from "@capacitor/local-notifications";
import { Capacitor } from "@capacitor/core";

let requested = false;
let notificationId = 1;

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
              id: notificationId++,
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
      new Notification(title, { body, icon: "/favicon.ico" });
    } catch {
      // ignore
    }
  }
}
