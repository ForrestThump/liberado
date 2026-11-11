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

function isIosUserAgent() {
  return /iPad|iPhone|iPod/.test(navigator.userAgent);
}

function isStandaloneDisplay() {
  if (typeof window.matchMedia === "function") {
    if (window.matchMedia("(display-mode: standalone)").matches) {
      return true;
    }
  }
  return !!navigator.standalone;
}

async function collectDeviceLabelHintsJson() {
  let platform = "";
  let brands = [];
  let model = "";
  let isBrave = false;
  if (navigator.userAgentData) {
    try {
      const data = await navigator.userAgentData.getHighEntropyValues([
        "platform",
        "model",
        "brands",
      ]);
      platform = data.platform || navigator.userAgentData.platform || "";
      model = data.model || "";
      if (Array.isArray(data.brands)) {
        brands = data.brands.map((b) => b.brand || b);
      }
    } catch (_e) {
      platform = navigator.userAgentData.platform || "";
    }
  }
  if (!platform && navigator.platform) {
    platform = navigator.platform;
  }
  if (navigator.brave && typeof navigator.brave.isBrave === "function") {
    try {
      isBrave = await navigator.brave.isBrave();
    } catch (_e) {
      isBrave = false;
    }
  }
  return JSON.stringify({
    user_agent: navigator.userAgent || "",
    platform,
    brands,
    model,
    is_brave: isBrave,
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
  let subscriptionEndpoint = "";

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
          subscriptionEndpoint = sub.endpoint || "";
          try {
            subscriptionHost = new URL(sub.endpoint).host;
          } catch (_e) {
            subscriptionHost = "";
          }
        }
      }
    }
  }

  const pushAvailable =
    typeof Notification !== "undefined" && "serviceWorker" in navigator;

  return JSON.stringify({
    notification_permission: notificationPermission,
    service_worker_state: serviceWorkerState,
    page_controlled: pageControlled,
    browser_subscribed: browserSubscribed,
    subscription_host: subscriptionHost,
    subscription_endpoint: subscriptionEndpoint,
    is_ios: isIosUserAgent(),
    display_standalone: isStandaloneDisplay(),
    push_available: pushAvailable,
  });
}

export async function fetchDeviceLabelHintsJson() {
  return collectDeviceLabelHintsJson();
}

export async function disablePushUnsubscribeJson() {
  if (!("serviceWorker" in navigator)) {
    return JSON.stringify({ endpoint: "" });
  }
  const reg = await navigator.serviceWorker.ready;
  if (!reg.pushManager) {
    return JSON.stringify({ endpoint: "" });
  }
  const sub = await reg.pushManager.getSubscription();
  if (!sub) {
    return JSON.stringify({ endpoint: "" });
  }
  const endpoint = sub.endpoint || "";
  await sub.unsubscribe();
  return JSON.stringify({ endpoint });
}

export async function upsertExistingPushSubscriptionJson(apiBase, label, csrfToken) {
  if (!("serviceWorker" in navigator)) {
    throw new Error("Service worker not available");
  }
  const reg = await serviceWorkerReadyWithTimeout();
  if (!reg.pushManager) {
    throw new Error("Push manager not available");
  }
  const sub = await reg.pushManager.getSubscription();
  if (!sub) {
    throw new Error("No browser push subscription to register");
  }
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
  return JSON.stringify({ endpoint: json.endpoint || "" });
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
