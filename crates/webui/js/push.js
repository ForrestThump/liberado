function urlBase64ToUint8Array(base64String) {
  const pad = "=".repeat((4 - (base64String.length % 4)) % 4);
  const base64 = (base64String + pad).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(base64);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

function csrfFromCookie() {
  const m = document.cookie.match(/liberado_csrf=([^;]+)/);
  return m ? decodeURIComponent(m[1]) : "";
}

const SW_READY_TIMEOUT_MS = 10000;

function serviceWorkerReadyWithTimeout() {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(
        new Error(
          "Service worker not ready within 10 seconds — reload the app or check that /sw.js is registered"
        )
      );
    }, SW_READY_TIMEOUT_MS);
    navigator.serviceWorker.ready.then(
      (reg) => {
        clearTimeout(timer);
        resolve(reg);
      },
      (err) => {
        clearTimeout(timer);
        reject(err);
      }
    );
  });
}

export async function fetchPushDeviceStateJson() {
  let notificationPermission = "unsupported";
  if (typeof Notification !== "undefined") {
    notificationPermission = Notification.permission;
  }

  let serviceWorkerState = "none";
  let pageControlled = false;
  let browserSubscribed = false;
  let subscriptionHost = "";

  if ("serviceWorker" in navigator) {
    pageControlled = !!navigator.serviceWorker.controller;
    const reg = await navigator.serviceWorker.getRegistration();
    if (reg) {
      if (reg.installing) {
        serviceWorkerState = "installing";
      } else if (reg.waiting) {
        serviceWorkerState = "waiting";
      } else if (reg.active) {
        serviceWorkerState = "active";
      } else {
        serviceWorkerState = "registered";
      }
      if (reg.pushManager) {
        const sub = await reg.pushManager.getSubscription();
        if (sub) {
          browserSubscribed = true;
          try {
            subscriptionHost = new URL(sub.endpoint).host;
          } catch (_e) {
            subscriptionHost = "";
          }
        }
      }
    }
  }

  return JSON.stringify({
    notification_permission: notificationPermission,
    service_worker_state: serviceWorkerState,
    page_controlled: pageControlled,
    browser_subscribed: browserSubscribed,
    subscription_host: subscriptionHost,
  });
}

export async function enablePushSubscribe(vapidKey, apiBase, label, csrfToken) {
  if (typeof Notification === "undefined") {
    throw new DOMException(
      "Notifications are not supported in this browser",
      "NotSupportedError"
    );
  }
  const perm = await Notification.requestPermission();
  if (perm !== "granted") {
    throw new DOMException(
      "Notification permission not granted: " + perm,
      "NotAllowedError"
    );
  }

  const reg = await serviceWorkerReadyWithTimeout();
  const sub = await reg.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: urlBase64ToUint8Array(vapidKey),
  });
  const json = sub.toJSON();
  const token = csrfToken || csrfFromCookie();
  const resp = await fetch(apiBase + "/api/push/subscriptions", {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      "x-liberado-csrf": token,
    },
    body: JSON.stringify({
      endpoint: json.endpoint,
      keys: json.keys,
      label: label,
    }),
  });
  if (!resp.ok) {
    const text = await resp.text();
    throw new Error("HTTP " + resp.status + ": " + text);
  }
  return { ok: true };
}
