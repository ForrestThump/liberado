import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createContext, runInContext } from "node:vm";

const dir = dirname(fileURLToPath(import.meta.url));
const swSource = readFileSync(join(dir, "sw.js"), "utf8");

assert.equal(
  swSource.includes("maxActions"),
  false,
  "do not feature-detect maxActions; that can hide the notification"
);

function loadWorker(hooks) {
  const listeners = {};
  const self = {
    skipWaiting() {},
    clients: hooks.clients,
    registration: {
      showNotification(title, options) {
        hooks.shown.push({ title, options });
        return Promise.resolve();
      },
    },
    addEventListener(type, fn) {
      listeners[type] = fn;
    },
  };
  const sandbox = {
    self,
    fetch: hooks.fetch,
    URL,
    console,
  };
  runInContext(swSource, createContext(sandbox), { filename: "sw.js" });
  return listeners;
}

async function runEvent(listener, event) {
  let pending = Promise.resolve();
  event.waitUntil = (promise) => {
    pending = promise;
  };
  listener(event);
  await pending;
}

function taskPayload() {
  return {
    title: "Due now: Call dentist",
    body: "Task due 2:00 PM",
    tag: "row-7",
    url: "/notifications/7",
    outbox_id: 7,
    snooze_minutes: 15,
    rows: [{ id: 7, token: "tok-7" }],
    actions: [
      { action: "done", title: "Done" },
      { action: "snooze", title: "Snooze 15m" },
    ],
  };
}

function harness(fetchImpl) {
  const shown = [];
  const calls = [];
  const opened = [];
  const navigated = [];
  const clients = [];
  const listeners = loadWorker({
    shown,
    clients: {
      claim() {
        return Promise.resolve();
      },
      matchAll() {
        return Promise.resolve(clients);
      },
      openWindow(url) {
        opened.push(url);
        return Promise.resolve();
      },
    },
    fetch: async (url, init) => {
      const body = init && init.body ? JSON.parse(init.body) : null;
      calls.push({ url, body });
      return fetchImpl(url, body);
    },
  });
  return { shown, calls, opened, navigated, clients, listeners };
}

function clickEvent(payload, action) {
  return {
    action: action,
    notification: {
      data: payload,
      close() {},
    },
  };
}

function pushEvent(payload) {
  return {
    data: {
      json() {
        return payload;
      },
    },
  };
}

const okJson = (notice) => async () => ({
  ok: true,
  status: 200,
  json: async () => ({ notice }),
});

await (async function pushShowsActionsOnlyForActionablePayloads() {
  const actionable = harness(okJson("unused"));
  await runEvent(actionable.listeners.push, pushEvent(taskPayload()));
  assert.equal(
    JSON.stringify(actionable.shown[0].options.actions),
    JSON.stringify([
      { action: "done", title: "Done" },
      { action: "snooze", title: "Snooze 15m" },
    ])
  );

  const plain = harness(okJson("unused"));
  await runEvent(
    plain.listeners.push,
    pushEvent({
      title: "Liberado test",
      body: "hello",
      tag: "test",
      url: "/notifications",
      rows: [{ id: 1, token: "t" }],
    })
  );
  assert.equal(plain.shown[0].options.actions, undefined);
  assert.equal(plain.shown[0].title, "Liberado test");
})();

await (async function bodyTapAcksAndNavigates() {
  const windowed = harness(async () => ({ ok: true, status: 204, json: async () => ({}) }));
  const payload = taskPayload();
  await runEvent(windowed.listeners.notificationclick, clickEvent(payload, ""));
  assert.deepEqual(
    windowed.calls.map((call) => call.url),
    ["/api/outbox/ack"]
  );
  assert.deepEqual(windowed.calls[0].body, { id: 7, token: "tok-7" });
  assert.deepEqual(windowed.opened, ["/notifications/7"]);
  assert.equal(windowed.shown.length, 0);

  const focused = harness(async () => ({ ok: true, status: 204, json: async () => ({}) }));
  focused.clients.push({
    async focus() {},
    async navigate(url) {
      focused.navigated.push(url);
    },
  });
  await runEvent(focused.listeners.notificationclick, clickEvent(payload, ""));
  assert.deepEqual(focused.navigated, ["/notifications/7"]);
  assert.deepEqual(focused.opened, []);
  assert.equal(focused.calls.filter((call) => call.url === "/api/outbox/action").length, 0);
})();

await (async function doneAndSnoozeSucceedWithoutOpeningAWindow() {
  const done = harness(okJson("Done: Due now: Call dentist"));
  done.clients.push({
    async focus() {
      throw new Error("body tap must not focus on success");
    },
    async navigate() {
      throw new Error("body tap must not navigate on success");
    },
  });
  await runEvent(done.listeners.notificationclick, clickEvent(taskPayload(), "done"));
  assert.deepEqual(done.calls, [
    {
      url: "/api/outbox/action",
      body: { id: 7, token: "tok-7", action: "done" },
    },
  ]);
  assert.deepEqual(done.opened, []);
  assert.equal(done.shown.length, 1);
  assert.equal(done.shown[0].title, "Done: Due now: Call dentist");
  assert.equal(done.shown[0].options.actions, undefined);
  assert.equal(done.shown[0].options.tag, "row-7");

  const snooze = harness(okJson("Snoozed until 3:15 PM"));
  await runEvent(snooze.listeners.notificationclick, clickEvent(taskPayload(), "snooze"));
  assert.deepEqual(snooze.calls[0].body, {
    id: 7,
    token: "tok-7",
    action: "snooze",
    minutes: 15,
  });
  assert.deepEqual(snooze.opened, []);
  assert.equal(snooze.shown[0].title, "Snoozed until 3:15 PM");
  assert.equal(snooze.shown[0].options.actions, undefined);
})();

await (async function actionFailureOpensTheDetailUrl() {
  const httpFail = harness(async (url) => {
    if (url === "/api/outbox/action") {
      return { ok: false, status: 409, json: async () => ({ error: "resolved" }) };
    }
    return { ok: true, status: 204, json: async () => ({}) };
  });
  await runEvent(httpFail.listeners.notificationclick, clickEvent(taskPayload(), "done"));
  assert.deepEqual(httpFail.opened, ["/notifications/7"]);
  assert.equal(
    httpFail.calls.some((call) => call.url === "/api/outbox/ack"),
    true
  );

  const offline = harness(async (url) => {
    if (url === "/api/outbox/action") {
      throw new Error("offline");
    }
    return { ok: true, status: 204, json: async () => ({}) };
  });
  await runEvent(offline.listeners.notificationclick, clickEvent(taskPayload(), "snooze"));
  assert.deepEqual(offline.opened, ["/notifications/7"]);
  assert.equal(offline.shown.length, 0);
})();
