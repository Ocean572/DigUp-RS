// Run with node --test shell/tests/*.test.cjs. No browser or model needed.
// The People tab's crop geometry and crop painting (shell/ui/faces.js): the same numbers the
// server UI pins (server/ui/src/components/digup-people.test.js — box [10,10,60,60] in a 200×100
// photo), the clamps, the width-0 fallback, and the scale-cancelling style values (FACES.md's crop
// law: `/render` scales the photo by pixels/max(W,H), the box rides source pixels, and the
// box→photo ratios cancel the scale exactly — the window is shown by enlarging W/cw and offsetting
// −cx0/cw, −cy0/ch. Pure geometry over relayed rows: no crop verbs, no invented numbers).
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

const source = readFileSync(join(__dirname, '../ui', 'faces.js'), 'utf8');

function peopleView() {
  const asked = [];
  const context = vm.createContext({
    element(tag, className) {
      return {
        tag, className, style: {}, children: [], attrs: {},
        append(...kids) { this.children.push(...kids); },
        setAttribute(key, value) { this.attrs[key] = value; },
      };
    },
    fAsk(request) {
      asked.push({ ...request });
      return Promise.resolve({ image: `/index/previews/render-${asked.length}.jpg` });
    },
    fURL: (path) => `asset://${path}`,
  });
  // The pure region: crop geometry + the render seam + the crop painter (faces.js MARKs).
  vm.runInContext(source.slice(source.indexOf('function cropGeometry('), source.indexOf('// MARK: relay')), context);
  return { context, asked };
}

const settle = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };

// The vm context has its own Object.prototype (cross-realm): assert/strict compares prototypes,
// so plain values travel home through JSON before they are compared.
const plain = (value) => JSON.parse(JSON.stringify(value));

// The server suite's COVER row (digup-people.test.js): box [10,10,60,60] in a 200×100 photo, the
// shoulders padding hitting the photo's bottom edge — cw 82.5 × ch 100, offsets 0.
const COVER = { id: 1, path: '/p/a.jpg', box: [10, 10, 60, 60], width: 200, height: 100 };
// A pre-schema-3 row: no map (width 0) — the crop window cannot exist, the whole-photo shows.
const PLAIN = { id: 2, path: '/p/b.jpg', box: [10, 10, 60, 60], width: 0, height: 0 };

test('the crop window is the engine’s box, padded and clamped to the photo (the server’s pinned numbers)', () => {
  const { context } = peopleView();
  assert.deepEqual(plain(context.cropGeometry(COVER)), { cw: 82.5, ch: 100, cx0: 0, cy0: 0, W: 200 });
  assert.equal(context.cropGeometry(PLAIN), null);
  assert.equal(context.cropGeometry({ ...COVER, box: [60, 10, 60, 60] }), null);   // x1 == x0: no window
  assert.equal(context.cropGeometry({ ...COVER, height: 0 }), null);
  // A face at the photo's edge: the padding clamps to the photo's own pixels, never past them.
  assert.deepEqual(plain(context.cropGeometry({ ...COVER, box: [170, 40, 190, 80] })), { cw: 38, ch: 84, cx0: 161, cy0: 16, W: 200 });
});

test('faceCrop asks the helper’s own render verb and paints the scale-cancelling style', async () => {
  const { context, asked } = peopleView();
  const box = context.faceCrop(COVER);
  await settle();
  assert.deepEqual(asked[0], { op: 'render', path: '/p/a.jpg', kind: 'image', loc: null, pixels: 256 });
  assert.equal(box.style.aspectRatio, '82.5 / 100');
  const img = box.children[0];
  assert.ok(img.src.startsWith('asset://'), img.src);
  assert.equal(img.style.width, `calc(100% * ${200 / 82.5})`);
  assert.equal(img.style.left, 'calc(-100% * 0)');
  assert.equal(img.style.top, 'calc(-100% * 0)');
});

test('a row without a map shows the whole photo — the fallback the server panel pins', async () => {
  const { context } = peopleView();
  const box = context.faceCrop(PLAIN);
  await settle();
  const img = box.children[0];
  assert.equal(img.className, 'plain');
  assert.equal(box.style.aspectRatio, undefined);
});
