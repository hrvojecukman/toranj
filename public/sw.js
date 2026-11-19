// Toranj mobile service worker: shows push notifications and opens the
// terminal a notification is about. Nothing is cached: the app always talks
// to the Mac live.
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("push", (e) => {
  let d = {};
  try { d = e.data ? e.data.json() : {}; } catch { d = { body: e.data ? e.data.text() : "" }; }
  const title = typeof d.title === "string" && d.title ? d.title : "Toranj";
  const tab = typeof d.tab === "string" ? d.tab : "";
  e.waitUntil(self.registration.showNotification(title, {
    body: typeof d.body === "string" ? d.body : "",
    tag: tab || "toranj",
    icon: "/m-icon-192.png",
    data: { tab },
  }));
});

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  const tab = (e.notification.data && e.notification.data.tab) || "";
  const url = tab ? `/#tab=${encodeURIComponent(tab)}` : "/";
  e.waitUntil((async () => {
    const all = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    for (const c of all) {
      if ("focus" in c) {
        c.postMessage({ open: tab });
        return c.focus();
      }
    }
    return self.clients.openWindow(url);
  })());
});
