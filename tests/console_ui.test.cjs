// Execute the shipped UI functions with controlled DOM and network responses.
const { readFileSync } = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const test = require('node:test');
const html = readFileSync('crates/sim-console/src/ui.html', 'utf8');
function source(name, next) {
  const start = html.indexOf(`async function ${name}(`);
  return html.slice(start, html.indexOf(next, start));
}
function context(code) {
  const elements = new Map();
  const calls = [];
  const c = vm.createContext({
    SELECTED: 'sim-a', LIVE_LABELS: { simulation: 'sim-a', fleet: {}, groups: {} },
    $: id => { if (!elements.has(id)) elements.set(id, { style: {} }); return elements.get(id); },
    fetch: async (...args) => { calls.push(args); return { ok: true, json: async () => ({ changed: [] }) }; },
    refresh: () => {}, alert: () => {}, setTimeout: () => {}, encodeURIComponent,
    loadSim: async () => {},
  });
  vm.runInContext(code, c);
  return { c, calls };
}
test('advance uses simulation-scoped backend route', async () => {
  const { c, calls } = context(source('doAdvance', '\nfunction renderBoard'));
  await c.doAdvance('disk-fill', 300);
  assert.equal(calls[0][0], '/api/sim/sim-a/scenario/disk-fill/advance');
  assert.equal(JSON.parse(calls[0][1].body).seconds, 300);
  const backend = readFileSync('crates/sim-console/src/main.rs', 'utf8');
  assert.ok(backend.includes('/api/sim/{name}/scenario/{scenario}/advance'));
});
test('advance surfaces HTTP failure', async () => {
  const { c } = context(source('doAdvance', '\nfunction renderBoard'));
  let error;
  c.fetch = async () => ({ ok: false, status: 404, json: async () => ({ error: 'not running' }) });
  c.alert = value => { error = value; };
  await c.doAdvance('disk-fill', 300);
  assert.match(error, /not running/);
});
test('labels cannot be sent to a different simulation', async () => {
  const { c, calls } = context(source('applyLiveLabels', '\nasync function'));
  c.SELECTED = 'sim-b';
  await c.applyLiveLabels();
  assert.equal(calls.length, 0);
});
test('selection invalidates loaded label editor', async () => {
  const { c } = context(source('selectSim', '\n/// Fetch'));
  await c.selectSim('sim-b');
  assert.equal(c.LIVE_LABELS, null);
  assert.equal(c.$('labelsApplyBtn').style.display, 'none');
});

test('readiness distinguishes operational status from verified demo evidence', () => {
  const start = html.indexOf('function renderBoard(');
  const { c } = context(html.slice(start, html.indexOf('function renderNodes(', start)));
  c.esc = value => value;
  c.$('boardWrap').dataset = {};
  for (const status of ['manual', 'warn']) {
    c.renderBoard({ operational_ready: true, demo_ready: false,
      checks: [{ name: 'Evidence', status, detail: 'Pending', remedy: 'Verify' }] });
    assert.equal(c.$('verdict').textContent, 'Operational; demo checks pending');
    assert.equal(c.$('verdict').className, 'verdict nogo');
  }
  c.renderBoard({ operational_ready: false, demo_ready: false, checks: [] });
  assert.equal(c.$('verdict').textContent, 'Not operational');
  c.renderBoard({ operational_ready: true, demo_ready: true, checks: [] });
  assert.equal(c.$('verdict').textContent, 'Demo ready');
  c.renderBoard(null);
  assert.equal(c.$('verdict').textContent, 'no simulation');
});
