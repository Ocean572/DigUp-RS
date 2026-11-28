// Run with node --test shell/tests/*.test.cjs. No browser or model needed.
// Find like this (#162/#121, 2026-11-25): the reference picture rides the daemon
// verb `find_similar` with its local path (the shell IS local — the render op is
// the server-supplied-path precedent); the engine's answer — hits, strong,
// found_from, warning — lands in the SAME rows machinery the query fills, and a
// late answer never overrides a newer query (the generation law, as `run`'s).
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

function findView() {
  const source = readFileSync(join(__dirname, '../ui/window.js'), 'utf8');
  const requests = [], renders = [];
  const context = vm.createContext({
    LIMIT: 40,
    renders,   // the context-side render() below closes over this by name
    FILTERS: [{ key: 'photos', kinds: ['image', 'screenshot'] }],
    FILTER_OF_KIND: { image: 'photos', screenshot: 'photos' },
    render() { renders.push('render'); },
    renderStatus() {},
    ask(request) {
      let resolve, reject;
      const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
      requests.push({ ...request, resolve, reject });
      return promise;
    },
  });
  // The real window.js pieces, verbatim: the state through the search flow
  // (its lexical scope runs through to the Drawing MARK), the extension
  // reader, rows(), and findLike itself.
  vm.runInContext(source.slice(source.indexOf('const state = {'),
                              source.indexOf('// MARK: Drawing')), context);
  vm.runInContext('function render() { renders.push("render"); }', context);
  vm.runInContext('function renderStatus() {}', context);
  vm.runInContext('function rows() { return [...state.ranked, ...(state.showWeaker ? state.weaker : [])]; }', context);
  vm.runInContext(source.slice(source.indexOf('async function findLike('),
                              source.indexOf('function failed(error)')), context);
  // The timer stubs run INSIDE the context: `const state` is a global lexical
  // binding there, not a sandbox property (context.state is undefined), and
  // only a script in the context can close over it.
  vm.runInContext('var setTimeout = (fn) => { state.timer = fn; return 1; };' +
                  'var clearTimeout = () => { state.timer = null; };', context);
  return {
    requests, renders,
    call(code) { return vm.runInContext(code, context); },
    state: vm.runInContext('state', context),
  };
}

const settle = async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); };

test('findLike sends the plain verb for the reference path and fills the rows', async () => {
  const f = findView();
  f.call('state.filter = null;');
  f.call('findLike({ path: "/data/photos/trip/IMG_1.jpg", kind: "image" })');
  const [all] = f.requests;
  assert.equal(all.op, 'find_similar');
  assert.equal(all.path, '/data/photos/trip/IMG_1.jpg');
  assert.equal(all.limit, 40);
  // The bar law (2026-11-27): the retired controls' fields never ride from the UI,
  // and the People dropdown does not pool a reference search — the plain verb.
  assert.equal(all.under, undefined);
  assert.equal(all.ext, undefined);
  assert.equal(all.person, undefined);
  assert.equal(all.q, undefined);   // the picture IS the query — no words ride
  all.resolve({ hits: [{ path: '/data/photos/trip/IMG_2.jpg', kind: 'image', score: 0.6, z: 3.1 },
                        { path: '/data/photos/trip/IMG_3.jpg', kind: 'image', score: 0.4 }],
                strong: 1, meaning: true,
                found_from: { path: '/data/photos/trip/IMG_1.jpg', kind: 'image' } });
  await settle();
  assert.equal(f.state.ranked.length, 1);
  assert.equal(f.state.weaker.length, 1);
  assert.deepEqual(f.state.findFrom, { path: '/data/photos/trip/IMG_1.jpg', kind: 'image' });
  assert.equal(f.state.hasResults, true);
  assert.equal(f.state.showWeaker, false);   // a new answer folds the weaker rows fresh
  assert.equal(f.renders.length, 1);
});

test('findLike rides the chip filter as a second verb, kinds on the answer', async () => {
  const f = findView();
  f.call('state.filter = "photos";');
  f.call('findLike({ path: "/data/a.png", kind: "screenshot" })');
  // The second verb only fires once the all-scope answer lands (findLike awaits
  // it first), so resolve that, settle, then read the second request.
  const [all] = f.requests;
  all.resolve({ hits: [], strong: 0, meaning: true });
  await settle();
  const shown = f.requests[1];
  assert.equal(shown.op, 'find_similar');
  assert.deepEqual([...shown.kinds], ['image', 'screenshot']);
  shown.resolve({ hits: [{ path: '/data/b.png', kind: 'screenshot', score: 0.5 }],
                   strong: 1, meaning: true,
                   found_from: { path: '/data/a.png', kind: 'screenshot' } });
  await settle();
  assert.equal(f.state.ranked.length, 1);
  assert.deepEqual(f.state.findFrom, { path: '/data/a.png', kind: 'screenshot' });
  // The daemon's warning rides the state verbatim — absent means null.
  assert.equal(f.state.warning, null);
});

test('the engine warning rides the answer verbatim', async () => {
  const f = findView();
  f.call('findLike({ path: "/data/deleted.jpg", kind: "image" })');
  f.requests[0].resolve({ hits: [], strong: 0, meaning: false,
                          warning: 'find like this: no such file: /data/deleted.jpg' });
  await settle();
  assert.equal(f.state.warning, 'find like this: no such file: /data/deleted.jpg');
  assert.equal(f.state.ranked.length, 0);
});

test('a document row declines the picture verb (no daemon call at all)', () => {
  const f = findView();
  f.call('findLike({ path: "/data/report.pdf", kind: "pdf" })');
  f.call('findLike({ path: "/data/song.mp3", kind: "audio" })');
  f.call('findLike(null)');
  assert.equal(f.requests.length, 0);
});

test('a late find-like answer cannot override a newer query (generation law)', async () => {
  const f = findView();
  f.call('findLike({ path: "/data/a.jpg", kind: "image" })');
  const [all] = f.requests;
  f.call('++state.generation;');   // a newer typing started; this answer is stale
  all.resolve({ hits: [{ path: '/data/x.jpg', kind: 'image', score: 0.9 }], strong: 1,
                meaning: true, found_from: { path: '/data/a.jpg', kind: 'image' } });
  await settle();
  assert.equal(f.state.ranked.length, 0);
  assert.equal(f.state.findFrom, null);
  assert.equal(f.renders.length, 0);
});

test('the shell source carries the Find-like seams (context menu, side actions, MOD+9, #151 label)', () => {
  const source = readFileSync(join(__dirname, '../ui/window.js'), 'utf8');
  assert.ok(source.includes('["Find like this", () => findLike(hit), `${MOD}9`]'));
  assert.ok(source.includes('findButton.title = `Find pictures like this one (${MOD}9)`'));
  assert.ok(source.includes('number === 9'));
  assert.ok(source.includes('findLike(state.hasResults && state.query.trim() ? selectedHit() : null)'));
  assert.ok(source.includes('only strong matches shown'));              // #151, status bar
  assert.ok(source.includes('only strong matches — rows standing out past its 2.0σ bar'));
  // The reference must be a picture: the kind guard on the verb's own door.
  assert.ok(source.includes('["image", "screenshot"].includes(hit.kind)'));
});
