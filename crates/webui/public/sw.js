"use strict";

// Thin worker: Chrome's install check wants a fetch handler. Chat is a live
// EventSource to /api/chat/stream, so this worker must not intercept /api/.
// It also does not cache. A cached index.html can pin an old hashed WASM.
// Push reminders always call showNotification (iOS 16.4+ installed PWA).
// Done and Snooze are notification actions when the payload includes them.
// A body tap still acks and opens payload.url. Platforms that hide actions
// keep that body tap.

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

function trayActions(payload) {
  var raw = payload && payload.actions;
  if (!Array.isArray(raw) || raw.length === 0) {
    return [];
  }
  var actions = [];
  for (var i = 0; i < raw.length; i++) {
    var item = raw[i];
    if (!item || !item.action || !item.title) {
      continue;
    }
    actions.push({ action: String(item.action), title: String(item.title) });
  }
  return actions;
}

function notificationOptions(payload, body, tag) {
  var options = {
    body: body,
    tag: tag,
    data: payload,
  };
  var actions = trayActions(payload);
  if (actions.length > 0) {
    options.actions = actions;
  }
  return options;
}

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
      await self.registration.showNotification(
        title,
        notificationOptions(payload, body, tag)
      );
    })()
  );
});

function actionRow(payload) {
  var rows = (payload && payload.rows) || [];
  var wanted = payload ? payload.outbox_id : null;
  if (wanted == null) {
    if (rows.length === 1 && rows[0] && rows[0].id != null && rows[0].token) {
      return rows[0];
    }
    return null;
  }
  for (var i = 0; i < rows.length; i++) {
    var row = rows[i];
    if (row && row.id === wanted && row.token) {
      return row;
    }
  }
  return null;
}

async function ackRows(rows) {
  var list = rows || [];
  for (var i = 0; i < list.length; i++) {
    var row = list[i];
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
}

async function focusOrOpen(targetUrl) {
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
}

async function openDetail(payload) {
  await ackRows(payload.rows || []);
  await focusOrOpen(payload.url || "/");
}

function fallbackNotice(action, payload) {
  var title = (payload && payload.title) || "Liberado";
  if (action === "snooze") {
    return "Snoozed";
  }
  return "Done: " + title;
}

async function postTrayAction(payload, action) {
  var row = actionRow(payload);
  if (!row) {
    return { ok: false };
  }
  var request = { id: row.id, token: row.token, action: action };
  if (action === "snooze" && typeof payload.snooze_minutes === "number") {
    request.minutes = payload.snooze_minutes;
  }
  var response = await fetch("/api/outbox/action", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
  });
  if (!response.ok) {
    return { ok: false };
  }
  var notice = fallbackNotice(action, payload);
  try {
    var parsed = await response.json();
    if (parsed && typeof parsed.notice === "string" && parsed.notice) {
      notice = parsed.notice;
    }
  } catch (e) {
    /* keep the fallback notice */
  }
  return { ok: true, notice: notice };
}

async function showConfirmation(payload, notice) {
  await self.registration.showNotification(notice, {
    tag: (payload && payload.tag) || "reminder",
    data: {
      url: (payload && payload.url) || "/",
      rows: (payload && payload.rows) || [],
      title: notice,
    },
  });
}

self.addEventListener("notificationclick", function (event) {
  event.notification.close();
  var payload = event.notification.data || {};
  var action = event.action || "";
  event.waitUntil(
    (async function () {
      if (action !== "done" && action !== "snooze") {
        await openDetail(payload);
        return;
      }
      try {
        var result = await postTrayAction(payload, action);
        if (!result.ok) {
          await openDetail(payload);
          return;
        }
        await showConfirmation(payload, result.notice);
      } catch (e) {
        await openDetail(payload);
      }
    })()
  );
});
