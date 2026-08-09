#!/usr/bin/env node
/**
 * Reproduce the git-subprocess hang that wedged the Paseo/ACP coding path.
 *
 * Symptom: every `git` the ACP bridge spawned blocked forever (~15ms CPU, then an
 * `Executive` wait). The same command from an interactive shell returns instantly, so the
 * variable is *how the process tree was launched*, not git and not the repo.
 *
 * Paseo launches the bridge from Node as:
 *     spawn('liberado-acp', { shell: true })   ->  cmd.exe /d /s /c "liberado-acp"  ->  git
 *
 * This script rebuilds that tree without Liberado in it at all, so a hang here proves the
 * problem is the launch environment rather than anything we wrote. Each case varies exactly
 * one thing; the git command is a read-only no-op (`worktree list`) plus the two writes the
 * bridge actually died on, so a case that passes has not changed repo state.
 *
 * Usage:  node scripts/repro-git-spawn-hang.js [--repo <path>] [--timeout <ms>]
 * Exit:   0 = every case completed, 1 = at least one hung (repro achieved)
 */

const { spawn } = require('node:child_process');
const path = require('node:path');

const args = process.argv.slice(2);
const argOf = (name, fallback) => {
  const i = args.indexOf(name);
  return i !== -1 && args[i + 1] ? args[i + 1] : fallback;
};

const REPO = path.resolve(argOf('--repo', process.cwd()));
const TIMEOUT_MS = Number(argOf('--timeout', '20000'));

// The two git binaries a Git-for-Windows install puts on PATH. `mingw64\bin` is the MSYS2
// build (msys-2.0.dll fork/tty emulation); `cmd\` is the thin native wrapper. Which one wins
// depends on PATH order, and the bridge inherits Paseo's PATH, not yours.
const GIT_MSYS = String.raw`C:\Program Files\Git\mingw64\bin\git.exe`;
const GIT_CMD = String.raw`C:\Program Files\Git\cmd\git.exe`;

/** `git worktree prune` is what the bridge hung on first, and is a no-op on a clean repo. */
const GIT_ARGS = ['-C', REPO, 'worktree', 'prune'];

/**
 * Run one case and report whether it finished inside the timeout.
 *
 * `stdio: 'pipe'` throughout: that is what Paseo does, and an unread pipe is one of the
 * things being tested, so the output is drained but not printed unless it is interesting.
 */
function runCase({ name, file, argv, useShell, detached, stdio }) {
  return new Promise((resolve) => {
    const started = Date.now();
    let child;
    try {
      // `shell: true` concatenates argv without escaping, so an unquoted "C:\Program Files\…"
      // is parsed by cmd.exe as the command `C:\Program` plus arguments. Quote it here rather
      // than dropping the case: "the shell path is broken" would masquerade as "no hang".
      const quoted = useShell && file.includes(' ') ? `"${file}"` : file;
      child = spawn(quoted, argv, {
        shell: useShell,
        detached: !!detached,
        stdio: stdio || ['pipe', 'pipe', 'pipe'],
        windowsHide: true,
      });
    } catch (e) {
      return resolve({ name, status: 'SPAWN-FAIL', ms: 0, detail: e.message });
    }

    let out = '';
    child.stdout?.on('data', (d) => (out += d));
    child.stderr?.on('data', (d) => (out += d));
    // Close stdin immediately: the bridge spawns git with a null stdin, and a child blocked
    // reading an inherited stdin would otherwise look identical to the hang we are chasing.
    child.stdin?.end();

    const timer = setTimeout(() => {
      // Kill the whole tree. The MSYS2 build re-execs itself, so killing the direct child
      // alone leaves a grandchild holding the pipes and the next case inherits the mess.
      try {
        if (process.platform === 'win32') {
          spawn('taskkill', ['/pid', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
        } else {
          child.kill('SIGKILL');
        }
      } catch {
        /* already gone */
      }
      resolve({ name, status: 'HUNG', ms: Date.now() - started, detail: out.trim() });
    }, TIMEOUT_MS);

    child.on('exit', (code) => {
      clearTimeout(timer);
      resolve({
        name,
        status: code === 0 ? 'ok' : `exit ${code}`,
        ms: Date.now() - started,
        detail: out.trim(),
      });
    });
  });
}

/**
 * Cases are ordered so the first failure localises the cause:
 *   1-2  which git binary, launched plainly from Node
 *   3-4  the same two through `cmd.exe /d /s /c`, which is what `shell: true` produces
 *   5    detached (no inherited console) -- the fix candidate if 3/4 hang
 */
const CASES = [
  { name: 'mingw64 git, direct from node', file: GIT_MSYS, argv: GIT_ARGS, useShell: false },
  { name: 'cmd\\git.exe, direct from node', file: GIT_CMD, argv: GIT_ARGS, useShell: false },
  { name: 'mingw64 git, via cmd.exe shell', file: GIT_MSYS, argv: GIT_ARGS, useShell: true },
  { name: 'cmd\\git.exe, via cmd.exe shell', file: GIT_CMD, argv: GIT_ARGS, useShell: true },
  { name: 'PATH `git`, via cmd.exe shell', file: 'git', argv: GIT_ARGS, useShell: true },
  { name: 'mingw64 git, detached (no console)', file: GIT_MSYS, argv: GIT_ARGS, useShell: false, detached: true },
];

(async () => {
  console.log(`repo:     ${REPO}`);
  console.log(`timeout:  ${TIMEOUT_MS}ms`);
  console.log(`node:     ${process.version} (pid ${process.pid})`);
  console.log(`command:  git ${GIT_ARGS.join(' ')}\n`);

  const results = [];
  for (const c of CASES) {
    process.stdout.write(`  ${c.name.padEnd(38)} ... `);
    const r = await runCase(c);
    console.log(`${r.status.padEnd(10)} ${r.ms}ms`);
    if (r.detail) console.log(`      ${r.detail.split('\n').join('\n      ')}`);
    results.push(r);
  }

  const hung = results.filter((r) => r.status === 'HUNG');
  console.log('');
  if (hung.length === 0) {
    console.log('No case hung. The launch tree alone does not reproduce it -- the next');
    console.log('variable to add is the bridge itself (see repro-acp-prompt.js).');
    process.exit(0);
  }
  console.log(`REPRODUCED: ${hung.length}/${results.length} case(s) hung:`);
  for (const h of hung) console.log(`  - ${h.name}`);
  process.exit(1);
})();
