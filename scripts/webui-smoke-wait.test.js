const assert = require('node:assert/strict');
const test = require('node:test');

const {
  waitForDebugTargets,
  chromeLaunchArgs,
  retryChromeStartup,
} = require('./webui-smoke.js');

test('a late Chrome answer is accepted', async () => {
  let probes = 0;
  const targets = await waitForDebugTargets(1, {
    attempts: 4,
    intervalMs: 0,
    pause: async () => {},
    probe: async () => {
      probes += 1;
      return probes < 3 ? 'starting' : '[{"type":"page"}]';
    },
  });
  assert.equal(targets[0].type, 'page');
  assert.equal(probes, 3);
});

test('a dead Chrome fails before the attempt budget', async () => {
  let probes = 0;
  await assert.rejects(
    () =>
      waitForDebugTargets(1, {
        attempts: 5,
        intervalMs: 0,
        pause: async () => {},
        alive: () => false,
        deadMessage: 'chrome died',
        probe: async () => {
          probes += 1;
          throw new Error('closed');
        },
      }),
    /chrome died/,
  );
  assert.equal(probes, 0);
});

test('a silent Chrome still fails when the budget ends', async () => {
  await assert.rejects(
    () =>
      waitForDebugTargets(9, {
        attempts: 2,
        intervalMs: 0,
        pause: async () => {},
        detail: () => 'DevTools refused the port',
        probe: async () => {
          throw new Error('closed');
        },
      }),
    /debug port 9 did not become ready[\s\S]*DevTools refused the port/,
  );
});

test('launch args pin the debug port to a fresh profile', () => {
  const args = chromeLaunchArgs(10080, 'http://127.0.0.1:9', '/tmp/profile');
  assert.ok(args.includes('--remote-debugging-address=127.0.0.1'));
  assert.ok(args.includes('--remote-debugging-port=10080'));
  assert.ok(args.includes('--user-data-dir=/tmp/profile'));
  assert.ok(args.includes('http://127.0.0.1:9/'));
});

test('a startup miss is tried once more', async () => {
  let calls = 0;
  const result = await retryChromeStartup(async () => {
    calls += 1;
    if (calls === 1) {
      throw new Error('Chrome debug port 1 did not become ready');
    }
    return 'ready';
  });
  assert.equal(result, 'ready');
  assert.equal(calls, 2);
});

test('a scenario failure is not launched again', async () => {
  let calls = 0;
  await assert.rejects(
    () =>
      retryChromeStartup(async () => {
        calls += 1;
        throw new Error('missing click target: notifications');
      }),
    /missing click target/,
  );
  assert.equal(calls, 1);
});
