// Native notifications and the Dock badge.
import { isPermissionGranted, requestPermission, sendNotification, onAction } from "@tauri-apps/plugin-notification";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

let allowed: boolean | null = null;

async function permitted(): Promise<boolean> {
  if (allowed !== null) return allowed;
  try {
    allowed = (await isPermissionGranted()) || (await requestPermission()) === "granted";
  } catch {
    allowed = false;
  }
  return allowed;
}

export type Kind = "waiting" | "done" | "error";

const TEXT: Record<Kind, string> = {
  waiting: "Waiting for your reply",
  done: "Done",
  error: "Finished with an error",
};

const log = (msg: string) => { invoke("log_ui", { msg }).catch(() => {}); };

/** macOS system sounds (/System/Library/Sounds). */
export const SOUNDS = ["Basso", "Blow", "Bottle", "Frog", "Funk", "Glass", "Hero", "Morse", "Ping", "Pop", "Purr", "Sosumi", "Submarine", "Tink"];

/** Plays a system sound in the app ("" plays nothing). */
export function playSound(name: string) {
  if (name) invoke("play_sound", { name }).catch(() => {});
}

/**
 * Sends a notification for a tab. `detail` is shown after the status text;
 * `sound` is the system sound to play, "" for none.
 */
export async function notifyTab(tab: string, title: string, kind: Kind, detail = "", sound = "") {
  // The app plays the sound itself: macOS keeps a notification (and its
  // sound) quiet while Toranj is the frontmost app, or when notifications
  // are off for it.
  playSound(sound);
  // Details can contain command lines or paths and stay in Notification
  // Center, so keep them short and on one line.
  // Waiting: only the tool name ("Bash"), the command itself stays in the app.
  const d = (kind === "waiting" ? detail.split(":")[0] : detail).split("\n")[0].trim();
  const shortDetail = d.length > 80 ? d.slice(0, 80) + "…" : d;
  const body = shortDetail ? `${TEXT[kind]}: ${shortDetail}` : TEXT[kind];
  // Paired phones too (Toranj decides by the phone settings and whether
  // anyone is at the Mac); never waits for it.
  // (The desktop test notification has no tab and stays on the Mac.)
  if (tab) void invoke("mobile_push", { tab, title, body, kind }).catch((e) => log(`mobile_push failed: ${e}`));
  // Native first: posted as Toranj, and a click opens the tab.
  const native = await invoke<boolean>("notify_native", { tab, title, body }).catch((e) => { log(`notify_native failed: ${e}`); return false; });
  log(`notify ${kind} native=${native}`);
  if (native) return;
  if (!(await permitted())) return;
  try {
    sendNotification({
      title,
      body,
      group: tab,
      extra: { tab },
    });
  } catch (e) {
    console.error("notification failed", e);
  }
}

/** Calls `open(tab)` when the user clicks a notification. */
export async function onNotificationClick(open: (tab: string) => void) {
  // Native notifications (the window is brought forward by the backend).
  listen<string>("notification-click", (e) => { if (typeof e.payload === "string") open(e.payload); }).catch(() => {});
  try {
    await onAction((e) => {
      const tab = e.notification?.extra?.tab;
      if (e.actionId !== "tap" || typeof tab !== "string") return;
      const win = getCurrentWindow();
      win.unminimize().then(() => win.setFocus()).catch(() => {});
      open(tab);
    });
  } catch {
    // Older platforms without click support: notifications still show.
  }
}

let lastBadge = -1;
export function setBadge(count: number) {
  if (count === lastBadge) return;
  lastBadge = count;
  getCurrentWindow().setBadgeCount(count > 0 ? count : undefined).catch(() => {});
}
