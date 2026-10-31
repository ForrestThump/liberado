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
const notFoundUrls = [];

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

function readRequestBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on('data', (chunk) => chunks.push(chunk));
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

function createStubState() {
  return {
    pushTestPostCount: 0,
    clientErrorReports: [],
    apiHits: [],
  };
}

async function handleApi(req, res, url, stubState) {
  const setCsrf = {
    'set-cookie': 'liberado_csrf=smoke-csrf; Path=/; SameSite=Lax',
  };
  stubState.apiHits.push({ method: req.method, path: url.pathname });

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
  if (url.pathname === '/api/models' && req.method === 'GET') {
    stubJson(res, 200, { current: null, models: [] }, setCsrf);
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
    stubJson(res, 200, [], setCsrf);
    return true;
  }
  if (url.pathname === '/api/push/vapid-public-key' && req.method === 'GET') {
    stubJson(res, 200, { public_key: VAPID_PUBLIC });
    return true;
  }
  if (url.pathname === '/api/push/test' && req.method === 'POST') {
    stubState.pushTestPostCount += 1;
    stubJson(res, 200, { ok: true }, setCsrf);
    return true;
  }
  if (url.pathname === '/api/push/subscriptions' && req.method === 'POST') {
    stubJson(res, 200, { ok: true }, setCsrf);
    return true;
  }
  if (url.pathname === '/api/client-errors' && req.method === 'POST') {
    const bodyText = await readRequestBody(req);
    try {
      stubState.clientErrorReports.push(JSON.parse(bodyText));
    } catch {
      stubState.clientErrorReports.push({ raw: bodyText });
    }
    stubJson(res, 204, {}, setCsrf);
    return true;
  }
  if (url.pathname.startsWith('/api/')) {
    stubJson(res, 404, { error: 'stub missing route', path: url.pathname });
    return true;
  }
  return false;
}

function startStaticServer(stubState) {
  if (!fs.existsSync(path.join(BUNDLE, 'index.html'))) {
    throw new Error(
      `bundle missing at ${BUNDLE} — run: dx build -r -p liberado-webui --web`,
    );
  }
  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url || '/', 'http://127.0.0.1');
    if (await handleApi(req, res, url, stubState)) {
      return;
    }
    let rel = decodeURIComponent(url.pathname);
    if (rel === '/' || rel === '') {
      rel = '/index.html';
    }
    if (rel === '/favicon.ico') {
      rel = '/icon-192.png';
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
    res.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
    res.end(`not found: ${rel}`);
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

async function evalString(cdp, expression) {
  const result = await cdp.send('Runtime.evaluate', {
    expression,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    throw new Error(JSON.stringify(result.exceptionDetails));
  }
  const value = result.result.value;
  return value == null ? '' : String(value);
}

async function clickAssert(cdp, expression, label) {
  let ok;
  try {
    ok = await evalBool(cdp, expression);
  } catch (err) {
    fail(`${label}: ${err.message}`);
    return false;
  }
  if (!ok) {
    fail(`missing click target: ${label}`);
    return false;
  }
  return true;
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

function notificationsMessageExpr() {
  return `(function(){
    const el = document.querySelector('.notifications-error');
    if (!el) return '';
    return (el.textContent || '').trim();
  })()`;
}

function notificationsMessageVisibleExpr() {
  return `(function(){
    const t = ${notificationsMessageExpr()};
    return t.length > 0 && t !== '{}';
  })()`;
}

async function runNormalFlow(cdp, stubState) {
  await waitFor(
    cdp,
    `!!document.querySelector('.phone-menu-fab, .sidebar-menu-btn')`,
    'menu control',
  );

  await clickAssert(
    cdp,
    `(function(){
      const btn = document.querySelector('.phone-menu-fab') || document.querySelector('.sidebar-menu-btn');
      if (!btn) return false;
      btn.click();
      return true;
    })()`,
    'hamburger / sidebar menu button',
  );
  await sleep(500);

  await clickAssert(
    cdp,
    `(function(){
      const row = document.querySelector('button[aria-label="Notifications"]');
      if (!row) return false;
      row.click();
      return true;
    })()`,
    'Notifications menu row',
  );

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

  await clickAssert(
    cdp,
    `(function(){
      const buttons = [...document.querySelectorAll('button')];
      const enable = buttons.find(b => b.textContent.includes('Enable push on this device'));
      if (!enable) return false;
      enable.click();
      return true;
    })()`,
    'Enable push on this device',
  );

  await waitFor(
    cdp,
    notificationsMessageVisibleExpr(),
    'visible status or error after Enable push',
    15_000,
  );

  const enableMsg = await evalString(cdp, notificationsMessageExpr());
  if (enableMsg === '{}' || enableMsg.length === 0) {
    fail(`Notifications message empty after Enable (got ${JSON.stringify(enableMsg)})`);
  }

  const pushTestBefore = stubState.pushTestPostCount;

  await clickAssert(
    cdp,
    `(function(){
      const buttons = [...document.querySelectorAll('button')];
      const test = buttons.find(b => b.textContent.includes('Send test'));
      if (!test) return false;
      test.click();
      return true;
    })()`,
    'Send test',
  );

  const sendStart = Date.now();
  while (Date.now() - sendStart < 15_000) {
    if (stubState.pushTestPostCount > pushTestBefore) {
      break;
    }
    const sendMsgNow = await evalString(cdp, notificationsMessageExpr());
    if (
      sendMsgNow.includes('Test notification queued') ||
      sendMsgNow.includes('Test send failed')
    ) {
      break;
    }
    await sleep(250);
  }

  await sleep(500);

  const pushTestHit = stubState.pushTestPostCount > pushTestBefore;
  const sendMsg = await evalString(cdp, notificationsMessageExpr());
  if (!pushTestHit && !sendMsg.includes('Test notification queued') && !sendMsg.includes('Test send failed')) {
    fail(
      `Send test: expected POST /api/push/test or visible outcome; posts=${stubState.pushTestPostCount}, message=${JSON.stringify(sendMsg)}`,
    );
  }

  const stillThere = await evalBool(
    cdp,
    `!!document.querySelector('.notifications-page h1')`,
  );
  if (!stillThere) {
    fail('Notifications page unmounted after interactions');
  }

  await clickAssert(
    cdp,
    `(function(){
      const btn = document.querySelector('.phone-menu-fab') || document.querySelector('.sidebar-menu-btn');
      if (!btn) return false;
      btn.click();
      return true;
    })()`,
    'hamburger / sidebar menu button (re-open after push flow)',
  );
  await sleep(400);

  await waitFor(
    cdp,
    `(function(){
      const layout = document.querySelector('.grok-layout');
      if (layout && layout.classList.contains('drawer-open')) return true;
      const btn = document.querySelector('.phone-menu-fab') || document.querySelector('.sidebar-menu-btn');
      return btn && btn.getAttribute('aria-expanded') === 'true';
    })()`,
    'sidebar drawer open after second menu click',
    10_000,
  );

  const drawerRow = await evalBool(
    cdp,
    `!!document.querySelector('button[aria-label="Notifications"]')`,
  );
  if (!drawerRow) {
    fail('Notifications row not visible in opened menu after push flow');
  }
}

async function runFatalGluePhase(cdp, stubState) {
  const reportsBefore = stubState.clientErrorReports.length;
  const glueFile = await evalString(
    cdp,
    `(function(){
      const scripts = [...document.querySelectorAll('script[src*="liberado-webui"]')];
      if (scripts.length) return scripts[scripts.length - 1].src;
      return location.origin + '/assets/liberado-webui-smoke.js';
    })()`,
  );

  const glueFileJson = JSON.stringify(glueFile);
  await cdp.send('Runtime.evaluate', {
    expression: `(function(){
      const glueFile = ${glueFileJson};
      const err = new TypeError("Cannot read properties of undefined (reading 'then')");
      err.stack = "TypeError: Cannot read properties of undefined (reading 'then')\\n    at __wbg_then_smoke (" + glueFile + ":1:1)";
      setTimeout(function(){ Promise.reject(err); }, 0);
      return true;
    })()`,
    awaitPromise: false,
  });

  await sleep(300);

  await waitFor(
    cdp,
    `!!document.getElementById('liberado-panic-banner')`,
    'liberado-panic-banner after simulated glue failure',
    10_000,
  );

  const reportStart = Date.now();
  let reported = false;
  while (Date.now() - reportStart < 10_000) {
    if (stubState.clientErrorReports.length > reportsBefore) {
      reported = true;
      break;
    }
    await sleep(200);
  }
  if (!reported) {
    fail('stub did not receive POST /api/client-errors after simulated glue failure');
    return;
  }

  const latest = stubState.clientErrorReports[stubState.clientErrorReports.length - 1];
  const msg =
    latest && typeof latest.message === 'string'
      ? latest.message
      : JSON.stringify(latest);
  if (!msg.includes("Cannot read properties of undefined (reading 'then')")) {
    fail(
      `client-errors payload missing glue TypeError message: ${JSON.stringify(latest)}`,
    );
  }
}

async function runScenario(origin, debugPort, stubState) {
  const cdp = await connectCdp(debugPort, origin);
  let phase = 'normal';

  cdp.onEvent((msg) => {
    if (msg.method === 'Network.responseReceived') {
      const status = msg.params.response.status;
      if (status === 404) {
        notFoundUrls.push(msg.params.response.url);
      }
    }
    if (msg.method === 'Runtime.exceptionThrown') {
      if (phase === 'fatal') {
        return;
      }
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
  await cdp.send('Network.enable');

  await cdp.send('Emulation.setDeviceMetricsOverride', {
    width: 412,
    height: 915,
    deviceScaleFactor: 1,
    mobile: true,
  });

  await cdp.send('Page.navigate', { url: `${origin}/` });
  await sleep(2000);

  await runNormalFlow(cdp, stubState);

  phase = 'fatal';
  await runFatalGluePhase(cdp, stubState);

  cdp.close();
}

function reportNotFoundUrls() {
  if (notFoundUrls.length === 0) {
    return;
  }
  console.log('HTTP 404 URLs:');
  const seen = new Set();
  for (const url of notFoundUrls) {
    if (seen.has(url)) {
      continue;
    }
    seen.add(url);
    console.log(`  ${url}`);
    try {
      const pathname = new URL(url).pathname;
      if (pathname.startsWith('/api/')) {
        fail(`unexpected API 404 (add stub or fix client): ${url}`);
      }
    } catch {
      fail(`malformed 404 URL: ${url}`);
    }
  }
}

async function main() {
  if (typeof WebSocket === 'undefined') {
    console.error(
      'FAIL: global WebSocket is unavailable. This smoke test needs Node.js 22+ (same as CI ubuntu-latest). Do not skip.',
    );
    process.exit(1);
  }

  if (!fs.existsSync(CHROME)) {
    throw new Error(`Chrome not found at ${CHROME} (set CHROME_BIN)`);
  }

  const stubState = createStubState();
  const { server, port, origin } = await startStaticServer(stubState);
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
    await runScenario(origin, debugPort, stubState);
  } finally {
    cleanup();
  }

  reportNotFoundUrls();

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
