const assert = require('node:assert/strict');
const test = require('node:test');

const { waitForDebugTargets } = require('./webui-smoke.js');

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
        probe: async () => {
          throw new Error('closed');
        },
      }),
    /debug port 9 did not become ready/,
  );
});
