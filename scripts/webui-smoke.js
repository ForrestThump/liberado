#!/usr/bin/env node
/**
 * Headless Chrome smoke test for the release WebUI bundle.
 *
 * Serves the dx release `public/` tree with stub `/api/*` handlers, drives the Notifications
 * flow over CDP, and fails on wasm-bindgen console noise or uncaught runtime exceptions.
 *
 * Usage: node scripts/webui-smoke.js [--bundle <path>] [--chrome <path>]
 */

const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');

const ROOT = path.resolve(__dirname, '..');
const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(name);
  return i !== -1 && argv[i + 1] ? argv[i + 1] : fallback;
};

const BUNDLE = path.resolve(
  opt('--bundle', path.join(ROOT, 'target/dx/liberado-webui/release/web/public')),
);
const CHROME = opt('--chrome', process.env.CHROME_BIN || '/usr/bin/google-chrome');
const VAPID_PUBLIC =
  'BEl62iUYgUihx49k1uTU32m-Yo3e_hQpQ5YkN8jS8M8r9s6k1uTU32m-Yo3e_hQpQ5YkN8jS8M8';

const failures = [];
const consoleErrors = [];

function fail(msg) {
  failures.push(msg);
  console.error(`FAIL: ${msg}`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function contentType(filePath) {
  const ext = path.extname(filePath).toLowerCase();
  switch (ext) {
    case '.html':
      return 'text/html; charset=utf-8';
    case '.js':
      return 'text/javascript; charset=utf-8';
    case '.wasm':
      return 'application/wasm';
    case '.json':
      return 'application/json; charset=utf-8';
    case '.css':
      return 'text/css; charset=utf-8';
    case '.svg':
      return 'image/svg+xml';
    case '.png':
      return 'image/png';
    case '.ico':
      return 'image/x-icon';
    default:
      return 'application/octet-stream';
  }
}

function stubJson(res, status, body, extraHeaders = {}) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'cache-control': 'no-store',
    ...extraHeaders,
  });
  res.end(payload);
}

function handleApi(req, res, url) {
  const setCsrf = {
    'set-cookie': 'liberado_csrf=smoke-csrf; Path=/; SameSite=Lax',
  };
  if (url.pathname === '/api/auth/session' && req.method === 'GET') {
    stubJson(
      res,
      200,
      { authenticated: true, login_required: false },
      setCsrf,
    );
    return true;
  }
  if (url.pathname === '/api/conversations' && req.method === 'GET') {
    stubJson(res, 200, [], setCsrf);
    return true;
  }
  if (url.pathname === '/api/projects' && req.method === 'GET') {
    stubJson(res, 200, [], setCsrf);
    return true;
  }
  if (url.pathname === '/api/profiles' && req.method === 'GET') {
    stubJson(res, 200, [], setCsrf);
    return true;
  }
  if (url.pathname === '/api/status' && req.method === 'GET') {
    stubJson(res, 200, {
      running: true,
      vault_path: '/tmp/vault',
      uptime_seconds: 1,
      watcher_active: false,
      dispatcher_attached: false,
      orchestrator_attached: false,
      reactions_seen: 0,
      enter_sends: true,
    });
    return true;
  }
  if (url.pathname === '/api/push/status' && req.method === 'GET') {
    stubJson(res, 200, {
      vapid_configured: true,
      subscription_count: 0,
      public_key: VAPID_PUBLIC,
    });
    return true;
  }
  if (url.pathname === '/api/push/subscriptions' && req.method === 'GET') {
    stubJson(res, 200, []);
    return true;
  }
  if (url.pathname === '/api/push/vapid-public-key' && req.method === 'GET') {
    stubJson(res, 200, { public_key: VAPID_PUBLIC });
    return true;
  }
  if (url.pathname === '/api/push/test' && req.method === 'POST') {
    stubJson(res, 200, { ok: true });
    return true;
  }
  if (url.pathname === '/api/push/subscriptions' && req.method === 'POST') {
    stubJson(res, 200, { ok: true });
    return true;
  }
  if (url.pathname === '/api/client-errors' && req.method === 'POST') {
    stubJson(res, 204, {});
    return true;
  }
  if (url.pathname.startsWith('/api/')) {
    stubJson(res, 404, { error: 'stub missing route', path: url.pathname });
    return true;
  }
  return false;
}

function startStaticServer() {
  if (!fs.existsSync(path.join(BUNDLE, 'index.html'))) {
    throw new Error(
      `bundle missing at ${BUNDLE} — run: dx build -r -p liberado-webui --web`,
    );
  }
  const server = http.createServer((req, res) => {
    const url = new URL(req.url || '/', 'http://127.0.0.1');
    if (handleApi(req, res, url)) {
      return;
    }
    let rel = decodeURIComponent(url.pathname);
    if (rel === '/' || rel === '') {
      rel = '/index.html';
    }
    const filePath = path.join(BUNDLE, rel.replace(/^\//, ''));
    const resolved = path.resolve(filePath);
    if (!resolved.startsWith(BUNDLE)) {
      res.writeHead(403);
      res.end('forbidden');
      return;
    }
    if (fs.existsSync(resolved) && fs.statSync(resolved).isFile()) {
      const data = fs.readFileSync(resolved);
      res.writeHead(200, { 'content-type': contentType(resolved) });
      res.end(data);
      return;
    }
    const index = path.join(BUNDLE, 'index.html');
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    res.end(fs.readFileSync(index));
  });
  return new Promise((resolve, reject) => {
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      resolve({ server, port, origin: `http://127.0.0.1:${port}` });
    });
    server.on('error', reject);
  });
}

class CdpSession {
  constructor(ws) {
    this.ws = ws;
    this.nextId = 1;
    this.pending = new Map();
    this.handlers = [];
    ws.addEventListener('message', (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id !== undefined) {
        const entry = this.pending.get(msg.id);
        if (entry) {
          this.pending.delete(msg.id);
          if (msg.error) {
            entry.reject(new Error(msg.error.message || JSON.stringify(msg.error)));
          } else {
            entry.resolve(msg.result);
          }
        }
        return;
      }
      for (const handler of this.handlers) {
        handler(msg);
      }
    });
  }

  onEvent(handler) {
    this.handlers.push(handler);
  }

  send(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  close() {
    this.ws.close();
  }
}

async function waitForDebugTargets(debugPort) {
  for (let attempt = 0; attempt < 40; attempt++) {
    try {
      const resp = await fetch(`http://127.0.0.1:${debugPort}/json/list`);
      const text = await resp.text();
      if (text.trim().startsWith('[')) {
        return JSON.parse(text);
      }
    } catch {
      // Chrome not listening yet.
    }
    await sleep(250);
  }
  throw new Error(`Chrome debug port ${debugPort} did not become ready`);
}

async function connectCdp(debugPort, pageUrl) {
  let targets = await waitForDebugTargets(debugPort);
  let target = targets.find(
    (t) => t.type === 'page' && t.url && t.url.startsWith(pageUrl),
  );
  if (!target) {
    const createResp = await fetch(
      `http://127.0.0.1:${debugPort}/json/new?${encodeURIComponent(`${pageUrl}/`)}`,
    );
    const text = await createResp.text();
    target = JSON.parse(text);
  }
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve);
    ws.addEventListener('error', reject);
  });
  return new CdpSession(ws);
}

async function evalBool(cdp, expression) {
  const result = await cdp.send('Runtime.evaluate', {
    expression,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    throw new Error(JSON.stringify(result.exceptionDetails));
  }
  return Boolean(result.result.value);
}

async function waitFor(cdp, expression, label, timeoutMs = 30_000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (await evalBool(cdp, expression)) {
      return;
    }
    await sleep(250);
  }
  throw new Error(`timeout waiting for ${label}`);
}

async function runScenario(origin, debugPort) {
  const cdp = await connectCdp(debugPort, origin);

  cdp.onEvent((msg) => {
    if (msg.method === 'Runtime.exceptionThrown') {
      const text = msg.params.exceptionDetails.text || '';
      const desc =
        msg.params.exceptionDetails.exception?.description || text;
      fail(`Runtime.exceptionThrown: ${desc}`);
    }
    if (msg.method === 'Log.entryAdded') {
      const entry = msg.params.entry;
      if (entry.level === 'error') {
        const line = entry.text || '';
        consoleErrors.push(line);
        if (line.includes('wasm-bindgen')) {
          fail(`console error contains wasm-bindgen: ${line}`);
        }
      }
    }
  });

  await cdp.send('Runtime.enable');
  await cdp.send('Log.enable');
  await cdp.send('Page.enable');

  await cdp.send('Emulation.setDeviceMetricsOverride', {
    width: 412,
    height: 915,
    deviceScaleFactor: 1,
    mobile: true,
  });

  await cdp.send('Page.navigate', { url: `${origin}/` });
  await sleep(2000);

  await waitFor(
    cdp,
    `!!document.querySelector('.phone-menu-fab, .sidebar-menu-btn')`,
    'menu control',
  );

  await cdp.send('Runtime.evaluate', {
    expression: `(function(){
      const btn = document.querySelector('.phone-menu-fab') || document.querySelector('.sidebar-menu-btn');
      if (btn) btn.click();
      return true;
    })()`,
  });
  await sleep(500);

  await cdp.send('Runtime.evaluate', {
    expression: `(function(){
      const row = document.querySelector('button[aria-label="Notifications"]');
      if (row) row.click();
      return !!row;
    })()`,
  });

  await waitFor(
    cdp,
    `!!document.querySelector('.notifications-page h1') && document.querySelector('.notifications-page h1').textContent.includes('Notifications')`,
    'Notifications page',
  );
  await sleep(1000);

  const banner = await evalBool(cdp, `!!document.getElementById('liberado-panic-banner')`);
  if (banner) {
    fail('panic reload banner appeared during Notifications load');
  }

  await cdp.send('Runtime.evaluate', {
    expression: `(function(){
      const buttons = [...document.querySelectorAll('button')];
      const enable = buttons.find(b => b.textContent.includes('Enable push on this device'));
      if (enable) enable.click();
      return !!enable;
    })()`,
  });
  await sleep(2000);

  await cdp.send('Runtime.evaluate', {
    expression: `(function(){
      const buttons = [...document.querySelectorAll('button')];
      const test = buttons.find(b => b.textContent.includes('Send test'));
      if (test) test.click();
      return !!test;
    })()`,
  });
  await sleep(1500);

  const stillThere = await evalBool(
    cdp,
    `!!document.querySelector('.notifications-page h1')`,
  );
  if (!stillThere) {
    fail('Notifications page unmounted after interactions');
  }

  cdp.close();
}

async function main() {
  if (!fs.existsSync(CHROME)) {
    throw new Error(`Chrome not found at ${CHROME} (set CHROME_BIN)`);
  }

  const { server, port, origin } = await startStaticServer();
  const debugPort = 9333 + (port % 1000);

  const chrome = spawn(
    CHROME,
    [
      '--headless=new',
      '--disable-gpu',
      '--no-sandbox',
      '--disable-dev-shm-usage',
      '--window-size=412,915',
      `--remote-debugging-port=${debugPort}`,
      `${origin}/`,
    ],
    { stdio: 'ignore' },
  );

  const cleanup = () => {
    server.close();
    chrome.kill('SIGKILL');
  };
  process.on('exit', cleanup);
  process.on('SIGINT', () => process.exit(1));

  await sleep(1500);

  try {
    await runScenario(origin, debugPort);
  } finally {
    cleanup();
  }

  if (consoleErrors.length) {
    console.log('console errors (non-fatal unless wasm-bindgen):');
    for (const line of consoleErrors) {
      console.log(`  ${line}`);
    }
  }

  if (failures.length) {
    console.error(`webui-smoke: ${failures.length} failure(s)`);
    process.exit(1);
  }
  console.log('webui-smoke: ok');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
