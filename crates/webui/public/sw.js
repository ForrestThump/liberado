"use strict";

// Thin worker: Chrome's install check wants a fetch handler. Chat is a live
// EventSource to /api/chat/stream, so this worker must not intercept /api/.
// It also does not cache. A cached index.html can pin an old hashed WASM.
// Push reminders always call showNotification (iOS 16.4+ installed PWA).

self.addEventListener("install", function () {
  self.skipWaiting();
});

self.addEventListener("activate", function (event) {
  event.waitUntil(self.clients.claim());
});

function isDaemonApi(urlString) {
  try {
    return new URL(urlString).pathname.startsWith("/api/");
  } catch (e) {
    return false;
  }
}

self.addEventListener("fetch", function (event) {
  if (isDaemonApi(event.request.url)) {
    return;
  }
  event.respondWith(fetch(event.request));
});

self.addEventListener("push", function (event) {
  event.waitUntil(
    (async function () {
      var payload = {};
      try {
        payload = event.data ? event.data.json() : {};
      } catch (e) {
        payload = {};
      }
      var title = payload.title || "Liberado";
      var body = payload.body || "";
      var tag = payload.tag || "reminder";
      await self.registration.showNotification(title, {
        body: body,
        tag: tag,
        data: payload,
      });
    })()
  );
});

self.addEventListener("notificationclick", function (event) {
  event.notification.close();
  var payload = event.notification.data || {};
  var rows = payload.rows || [];
  var targetUrl = payload.url || "/";
  event.waitUntil(
    (async function () {
      for (var i = 0; i < rows.length; i++) {
        var row = rows[i];
        if (!row || row.id == null || !row.token) {
          continue;
        }
        try {
          await fetch("/api/outbox/ack", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ id: row.id, token: row.token }),
          });
        } catch (e) {
          /* best effort */
        }
      }
      var clients = await self.clients.matchAll({
        type: "window",
        includeUncontrolled: true,
      });
      for (var j = 0; j < clients.length; j++) {
        var client = clients[j];
        if ("focus" in client) {
          await client.focus();
          if (targetUrl && "navigate" in client) {
            try {
              await client.navigate(targetUrl);
            } catch (e) {
              /* ignore */
            }
          }
          return;
        }
      }
      if (self.clients.openWindow) {
        await self.clients.openWindow(targetUrl);
      }
    })()
  );
});
