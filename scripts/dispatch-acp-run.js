#!/usr/bin/env node
/**
 * Dispatch one coding run through the ACP bridge — the same path Paseo drives.
 *
 * ## Why this rather than the headless runner
 *
 * `liberado-coder-run task run` is simpler and needs no JSON-RPC. It is also a *different path*,
 * and the difference is the whole point. Three fixes in one week landed on the ACP path and not
 * its headless sibling — `preserve_work`, inherited stdin, and the real verifiers — so a run
 * dispatched through the runner does not exercise what a Paseo user actually gets. Streaming,
 * worktree path-dep provisioning, auto-commit on exit, and the review findings all live here.
 *
 * ## Spawn shape
 *
 * `shell: true` by default because that is what Paseo does (`cmd.exe /d /s /c "liberado-acp"`).
 * `--no-shell` removes cmd.exe from the chain. Both were verified to complete the handshake; the
 * flag exists because the difference has mattered before, when every child git inherited the
 * JSON-RPC stdin and blocked forever.
 *
 * ## What this costs
 *
 * A real model run against whatever key is in the environment, and a real branch in the repo.
 * `session/new` alone is free — the worktree is not built until the first prompt — so
 * `--handshake-only` is a safe way to check the bridge is alive.
 *
 * Usage:
 *   node scripts/dispatch-acp-run.js --cwd <repo> --prompt "..." [--mode coding]
 *                                    [--timeout-min 45] [--no-shell] [--handshake-only]
 *                                    [--bin <path>] [--config-dir <dir>] [--json]
 *
 * Exit: 0 = the run returned, 1 = it timed out or failed, 2 = handshake failed.
 */

const { spawn } = require('node:child_process');
const path = require('node:path');
const os = require('node:os');

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);
const opt = (name, fallback) => {
  const i = argv.indexOf(name);
  return i !== -1 && argv[i + 1] ? argv[i + 1] : fallback;
};

const BIN = opt('--bin', path.join(os.homedir(), '.cargo', 'bin', 'liberado-acp.exe'));
const CWD = path.resolve(opt('--cwd', process.cwd()));
const PROMPT = opt('--prompt', null);
const MODE = opt('--mode', null);
const TIMEOUT_MS = Number(opt('--timeout-min', '45')) * 60_000;
const USE_SHELL = !flag('--no-shell');
const HANDSHAKE_ONLY = flag('--handshake-only');
const AS_JSON = flag('--json');

if (!PROMPT && !HANDSHAKE_ONLY) {
  console.error('--prompt is required (or pass --handshake-only)');
  process.exit(2);
}

const log = (...a) => {
  if (!AS_JSON) console.log(...a);
};

// Point the bridge at a config directory, the way Paseo's provider entry does.
//
// This script inherited the environment and nothing set LIBERADO_CONFIG_DIR, so every run
// dispatched through it read no topology at all: no declared project, therefore no ship bar, and
// no policy grant. The bridge is installed to ~/.cargo/bin, so its own discovery — walk up from
// the binary for a `config/`, or look in the platform config dir — finds nothing either. A
// dogfood run that does not load the deployment's config is not testing the deployment.
//
// `--config-dir` overrides; an existing environment value still wins, so this cannot quietly
// redirect a caller who set one deliberately.
const CONFIG_DIR = opt('--config-dir', path.join(CWD, 'config'));
const childEnv = { ...process.env };
if (!childEnv.LIBERADO_CONFIG_DIR) childEnv.LIBERADO_CONFIG_DIR = CONFIG_DIR;

const child = spawn(USE_SHELL ? `"${BIN}"` : BIN, [], {
  cwd: CWD,
  shell: USE_SHELL,
  env: childEnv,
  stdio: ['pipe', 'pipe', 'pipe'],
  windowsHide: true,
});
child.on('error', (e) => {
  console.error(`spawn failed: ${e.message}`);
  process.exit(2);
});

let stderrBuf = '';
child.stderr.on('data', (d) => (stderrBuf += d));

// NDJSON both ways.
let buf = '';
const waiters = new Map();
const updates = [];
child.stdout.on('data', (d) => {
  buf += d;
  let nl;
  while ((nl = buf.indexOf('\n')) !== -1) {
    const line = buf.slice(0, nl).trim();
    buf = buf.slice(nl + 1);
    if (!line) continue;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      log(`[non-json] ${line}`);
      continue;
    }
    if (msg.id !== undefined && waiters.has(msg.id)) {
      waiters.get(msg.id)(msg);
      waiters.delete(msg.id);
    } else if (msg.method === 'session/update') {
      updates.push(msg.params);
      render(msg.params?.update);
    } else if (msg.method) {
      // A request from the agent (permission, for instance). Nothing here can answer one, so
      // say so loudly rather than letting the run hang on a prompt no human will see.
      log(`[agent -> client] ${msg.method}`);
      if (msg.id !== undefined) {
        child.stdin.write(
          `${JSON.stringify({
            jsonrpc: '2.0',
            id: msg.id,
            error: { code: -32601, message: 'unattended dispatcher cannot answer requests' },
          })}\n`,
        );
      }
    }
  }
});

/** Print the stream the way a human watching Paseo would see it. */
function render(update) {
  if (!update) return;
  const kind = update.sessionUpdate;
  if (kind === 'agent_message_chunk') {
    const text = update.content?.text ?? '';
    if (text.trim()) log(text.trimEnd());
  } else if (kind === 'tool_call') {
    log(`  · ${update.title ?? update.toolCallId ?? 'tool'}`);
  } else if (kind === 'tool_call_update') {
    if (update.status && update.status !== 'in_progress') {
      log(`    ${update.status}`);
    }
  }
}

let nextId = 1;
const call = (method, params, timeoutMs) =>
  new Promise((resolve, reject) => {
    const id = nextId++;
    const timer = setTimeout(() => reject(new Error(`timeout waiting for '${method}'`)), timeoutMs);
    waiters.set(id, (m) => {
      clearTimeout(timer);
      resolve(m);
    });
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
  });

function finish(code, payload) {
  if (AS_JSON) console.log(JSON.stringify(payload, null, 2));
  try {
    child.kill();
  } catch {
    /* already gone */
  }
  process.exit(code);
}

(async () => {
  log(`bin:     ${BIN}`);
  log(`cwd:     ${CWD}`);
  log(`shell:   ${USE_SHELL}`);
  log(`timeout: ${Math.round(TIMEOUT_MS / 60000)} min\n`);

  const init = await call('initialize', { protocolVersion: 1 }, 20_000).catch((e) => {
    console.error(`initialize failed: ${e.message}`);
    if (stderrBuf.trim()) console.error(stderrBuf.trim());
    process.exit(2);
  });
  log(`agent:   ${init.result?.agentInfo?.name} ${init.result?.agentInfo?.version}`);

  const session = await call('session/new', { cwd: CWD }, 120_000);
  if (session.error) finish(2, { error: session.error });
  const sessionId = session.result?.sessionId;
  log(`session: ${sessionId}\n`);

  if (MODE) {
    const set = await call('session/set_mode', { sessionId, modeId: MODE }, 30_000);
    if (set.error) log(`(mode '${MODE}' rejected: ${set.error.message})`);
  }

  if (HANDSHAKE_ONLY) {
    finish(0, { ok: true, sessionId, agent: init.result?.agentInfo });
  }

  const started = Date.now();
  try {
    const res = await call(
      'session/prompt',
      { sessionId, prompt: [{ type: 'text', text: PROMPT }] },
      TIMEOUT_MS,
    );
    const mins = ((Date.now() - started) / 60000).toFixed(1);
    log(`\n--- returned in ${mins} min ---`);
    if (res.error) finish(1, { sessionId, error: res.error, updates: updates.length });
    finish(0, {
      sessionId,
      stopReason: res.result?.stopReason,
      minutes: Number(mins),
      updates: updates.length,
    });
  } catch (e) {
    // Cancel before giving up. The bridge commits whatever the worktree holds on cancel, so a
    // dispatcher that just exits throws away the run's output; one that cancels keeps it.
    log(`\n${e.message} — cancelling so the work is committed`);
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'session/cancel', params: { sessionId } })}\n`);
    await new Promise((r) => setTimeout(r, 15_000));
    if (stderrBuf.trim()) console.error(`\nbridge stderr:\n${stderrBuf.trim().slice(-2000)}`);
    finish(1, { sessionId, error: 'timeout', updates: updates.length });
  }
})();
