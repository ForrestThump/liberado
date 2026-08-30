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
