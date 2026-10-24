"use strict";

// Thin worker: Chrome's install check wants a fetch handler. Chat is a live
// EventSource to /api/chat/stream, so this worker must not intercept /api/.
// It also does not cache. A cached index.html can pin an old hashed WASM.

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
  var payload = { title: "Liberado", body: "", data: { url: "/" } };
  if (event.data) {
    try {
      payload = event.data.json();
    } catch (e) {
      payload.body = event.data.text();
    }
  }
  var title = payload.title || "Liberado";
  var options = {
    body: payload.body || "",
    data: payload.data || { url: "/" },
  };
  event.waitUntil(self.registration.showNotification(title, options));
});

self.addEventListener("notificationclick", function (event) {
  event.notification.close();
  var target = "/";
  if (event.notification.data && event.notification.data.url) {
    target = event.notification.data.url;
  }
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then(function (list) {
      for (var i = 0; i < list.length; i++) {
        if (list[i].focus) {
          return list[i].focus();
        }
      }
      if (self.clients.openWindow) {
        return self.clients.openWindow(target);
      }
    })
  );
});
