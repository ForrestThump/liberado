#!/usr/bin/env node
/**
 * Drive `liberado-acp` over stdio JSON-RPC exactly the way Paseo does, to reproduce the
 * git-subprocess hang outside the editor.
 *
 * Background: a Paseo coding prompt wedged for 19 minutes without ever calling a model. Every
 * `git` the bridge spawned blocked (~15ms CPU, then an `Executive` wait) -- first
 * `worktree prune`, then `worktree add`. Killing a child let the bridge advance to the next
 * command, which hung identically, so the bridge's own await machinery is fine and git is the
 * thing that never answers.
 *
 * `repro-git-spawn-hang.js` already showed the launch tree alone is innocent: node -> cmd.exe
 * -> git completes in ~30ms for every git binary on this box. So the remaining variable is the
 * bridge process itself, and this script adds exactly that and nothing else -- same spawn shape
 * Paseo uses (`shell: true`, piped stdio, hidden window), same handshake, same first prompt.
 *
 * It stops at the point of interest: the coding path calls `ensure_session_worktree` *before*
 * any model call, so a hang reproduces with no API key and no tokens spent.
 *
 * Usage:
 *   node scripts/repro-acp-prompt.js [--bin <path>] [--cwd <repo>] [--timeout <ms>] [--no-shell]
 *                                    [--git-trace <file>] [--env K=V ...]
 *
 * `--git-trace` is the one that answers "where does git stop": GIT_TRACE is inherited down the
 * whole tree, and pointing it at a *file* sidesteps the fact that the bridge captures git's
 * stderr and then discards it (`let _ = ...output().await`), which is why the hang has been
 * mute so far.
 *
 * Exit: 0 = the prompt returned (no repro), 1 = hung (repro), 2 = handshake failed.
 */

const { spawn, spawnSync } = require('node:child_process');
const path = require('node:path');
const os = require('node:os');

const args = process.argv.slice(2);
const argOf = (name, fallback) => {
  const i = args.indexOf(name);
  return i !== -1 && args[i + 1] ? args[i + 1] : fallback;
};
const has = (name) => args.includes(name);

const BIN = argOf('--bin', path.join(os.homedir(), '.cargo', 'bin', 'liberado-acp.exe'));
const CWD = path.resolve(argOf('--cwd', process.cwd()));
const TIMEOUT_MS = Number(argOf('--timeout', '60000'));
// Paseo uses shell:true. --no-shell removes cmd.exe from the chain, which is one of the
// variables worth flipping once a hang reproduces.
const USE_SHELL = !has('--no-shell');

// Extra environment for the bridge (and therefore for every git it spawns).
const childEnv = { ...process.env };
const GIT_TRACE_FILE = argOf('--git-trace', null);
if (GIT_TRACE_FILE) {
  const abs = path.resolve(GIT_TRACE_FILE);
  // GIT_TRACE only writes to a file when given an absolute path; a relative one is treated as
  // a boolean and goes to stderr, which the bridge throws away.
  childEnv.GIT_TRACE = abs;
  childEnv.GIT_TRACE_SETUP = abs;
  childEnv.GIT_TRACE_PERFORMANCE = abs;
}
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--env' && args[i + 1]) {
    const eq = args[i + 1].indexOf('=');
    if (eq > 0) childEnv[args[i + 1].slice(0, eq)] = args[i + 1].slice(eq + 1);
  }
}

/** Snapshot the bridge's git descendants, so a hang names the exact command it died on. */
function gitDescendants(rootPid) {
  if (process.platform !== 'win32') return [];
  // Every process, not just git.exe. Filtering to git *before* building the tree breaks the
  // walk: the chain is cmd.exe -> liberado-acp -> git, and liberado-acp is not a git process,
  // so a git-only map has no path to the children and always reports "none found" -- which
  // reads as evidence that git is innocent rather than as a broken query.
  const ps = spawnSync(
    'powershell',
    [
      '-NoProfile',
      '-Command',
      `Get-CimInstance Win32_Process | ` +
        `Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json -Compress`,
    ],
    { encoding: 'utf8' },
  );
  if (ps.status !== 0 || !ps.stdout.trim()) return [];
  let rows;
  try {
    rows = JSON.parse(ps.stdout);
  } catch {
    return [];
  }
  if (!Array.isArray(rows)) rows = [rows];
  // Walk down from the bridge: git re-execs itself, so the interesting one is any descendant.
  const byParent = new Map();
  for (const r of rows) {
    if (!byParent.has(r.ParentProcessId)) byParent.set(r.ParentProcessId, []);
    byParent.get(r.ParentProcessId).push(r);
  }
  // Walk the whole descendant tree; report only the git processes in it, but keep every
  // non-git process as a link so the walk can reach them.
  const found = [];
  const seen = new Set();
  const walk = (pid, depth) => {
    if (seen.has(pid)) return; // defend against a pid-reuse cycle hanging the poller
    seen.add(pid);
    for (const r of byParent.get(pid) || []) {
      if (r.Name === 'git.exe') found.push({ ...r, depth });
      walk(r.ProcessId, depth + 1);
    }
  };
  walk(rootPid, 0);
  return found;
}

function killTree(pid) {
  if (!pid) return;
  try {
    if (process.platform === 'win32') {
      spawnSync('taskkill', ['/pid', String(pid), '/T', '/F'], { stdio: 'ignore' });
    } else {
      process.kill(pid, 'SIGKILL');
    }
  } catch {
    /* already gone */
  }
}

(async () => {
  console.log(`bin:      ${BIN}`);
  console.log(`cwd:      ${CWD}`);
  console.log(`shell:    ${USE_SHELL} ${USE_SHELL ? '(cmd.exe /d /s /c -- same as Paseo)' : '(direct)'}`);
  console.log(`timeout:  ${TIMEOUT_MS}ms\n`);

  if (GIT_TRACE_FILE) console.log(`git trace: ${path.resolve(GIT_TRACE_FILE)}\n`);

  const child = spawn(USE_SHELL ? `"${BIN}"` : BIN, [], {
    cwd: CWD,
    shell: USE_SHELL,
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
    env: childEnv,
  });

  let stderrBuf = '';
  child.stderr.on('data', (d) => (stderrBuf += d));

  // NDJSON in, NDJSON out.
  let buf = '';
  const waiters = new Map();
  const notifications = [];
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
        console.log(`  [non-json] ${line}`);
        continue;
      }
      if (msg.id !== undefined && waiters.has(msg.id)) {
        waiters.get(msg.id)(msg);
        waiters.delete(msg.id);
      } else if (msg.method) {
        notifications.push(msg);
        const kind = msg.params?.update?.sessionUpdate || msg.method;
        console.log(`  <- notification: ${kind}`);
      }
    }
  });

  let nextId = 1;
  const call = (method, params, timeoutMs) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      const timer = setTimeout(
        () => reject(new Error(`timeout after ${timeoutMs}ms waiting for '${method}'`)),
        timeoutMs,
      );
      waiters.set(id, (m) => {
        clearTimeout(timer);
        resolve(m);
      });
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
    });

  const fail = (code, msg) => {
    console.error(`\n${msg}`);
    if (stderrBuf.trim()) console.error(`\nbridge stderr:\n${stderrBuf.trim()}`);
    killTree(child.pid);
    process.exit(code);
  };

  try {
    console.log('-> initialize');
    const init = await call('initialize', { protocolVersion: 1 }, 15000);
    if (init.error) return fail(2, `initialize failed: ${JSON.stringify(init.error)}`);
    console.log(`   protocolVersion=${init.result?.protocolVersion}\n`);

    console.log(`-> session/new (cwd=${CWD})`);
    const sess = await call('session/new', { cwd: CWD }, 30000);
    if (sess.error) return fail(2, `session/new failed: ${JSON.stringify(sess.error)}`);
    const sessionId = sess.result?.sessionId;
    if (!sessionId) return fail(2, `session/new returned no sessionId: ${JSON.stringify(sess.result)}`);
    console.log(`   sessionId=${sessionId}  mode=${sess.result?.mode?.currentModeId ?? '?'}\n`);

    // The coding path builds the worktree before any model call, so this reaches the suspect
    // code without spending a token. Keep the text trivial for the same reason.
    console.log('-> session/prompt (coding; reaches ensure_session_worktree before any model call)');
    const started = Date.now();
    const promptPromise = call(
      'session/prompt',
      { sessionId, prompt: [{ type: 'text', text: 'print the current branch name' }] },
      TIMEOUT_MS,
    );

    // Poll for git descendants while we wait -- this is the evidence that matters.
    const poll = setInterval(() => {
      const gits = gitDescendants(child.pid);
      const secs = Math.round((Date.now() - started) / 1000);
      if (gits.length) {
        console.log(`   [${secs}s] ${gits.length} git descendant(s):`);
        for (const g of gits) console.log(`      pid ${g.ProcessId}: ${g.CommandLine}`);
      } else {
        console.log(`   [${secs}s] no git descendants`);
      }
    }, 10000);

    try {
      const res = await promptPromise;
      clearInterval(poll);
      const ms = Date.now() - started;
      console.log(`\nprompt returned in ${ms}ms: ${JSON.stringify(res.result ?? res.error)}`);
      console.log(`notifications received: ${notifications.length}`);
      console.log('\nNO REPRO -- the prompt completed.');
      killTree(child.pid);
      process.exit(0);
    } catch (e) {
      clearInterval(poll);
      const gits = gitDescendants(child.pid);
      console.log(`\nREPRODUCED: ${e.message}`);
      console.log(`notifications received: ${notifications.length} (a coding run that never`);
      console.log('starts emits none, which is why Paseo shows only a spinner)');
      if (gits.length) {
        console.log('\nhung git descendants at timeout:');
        for (const g of gits) console.log(`  pid ${g.ProcessId} (depth ${g.depth}): ${g.CommandLine}`);
      } else {
        console.log('\nNo git descendants -- the bridge is stuck somewhere other than a git spawn.');
      }
      if (stderrBuf.trim()) console.log(`\nbridge stderr:\n${stderrBuf.trim()}`);
      killTree(child.pid);
      process.exit(1);
    }
  } catch (e) {
    return fail(2, `handshake error: ${e.message}`);
  }
})();
