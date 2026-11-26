// Run with node --test shell/tests/*.test.cjs. No browser or model needed.
// The folder selection (setup.js): what the indexer gets. Issue #1's law: every chosen
// folder stays a root — nested picks included. The engine walks overlapping roots once
// per path (foundRoot/seen guards) and exempts chosen roots from the dataset and
// code-project rules; dropping a nested pick in the UI made the promise "a chosen folder
// is read whole" unexpressible (a numbered wallpaper collection ticked inside its parent
// was swept as a dataset, because the engine never learned it was picked).
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

function selectionView() {
  const source = readFileSync(join(__dirname, '../ui/setup.js'), 'utf8').replace(/\nstart\(\);\s*$/, '\n');
  const context = vm.createContext({
    window: { __TAURI__: { core: { invoke: async () => ({}) }, event: { listen: async () => ({}) } } },
    navigator: { platform: "Linux" },
    location: { search: "" },
    document: {
      // Browser-shaped element stubs: setup.js binds its page at load (the composition
      // harness's own pattern) — clicks are stored, nothing is touched.
      getElementById: () => ({ addEventListener: () => {} }),
      querySelectorAll: () => [],
      addEventListener: () => {},
    },
    setInterval: () => 0,
    setTimeout: () => 0,
    URLSearchParams, // the browser has it as a global; a bare vm context does not
  });
  // The trailing expression is the script's completion value: hand the selection out.
  return vm.runInContext(source + '\n({ selection, unique });', context);
}

test("nested chosen folders stay roots (issue #1)", () => {
  const { selection } = selectionView();
  selection.chosen = ["/data/photos", "/data/photos/collection"];
  selection.skipped = [];
  assert.deepEqual(selection.folders().slice().sort(),
    ["/data/photos", "/data/photos/collection"]);
  assert.deepEqual(selection.skips(), []);

  // Duplicates drop; the chosen set stays the person's own, in order.
  selection.chosen = ["/data/photos", "/data/photos", "/data/other"];
  assert.deepEqual(selection.folders(), ["/data/photos", "/data/other"]);

  // Skips inside a searched root keep reporting (unchanged behavior): the exclusions
  // ride --exclude, the roots stay the chosen ones.
  selection.chosen = ["/data/photos"];
  selection.skipped = ["/data/photos/secret", "/elsewhere/one"];
  assert.deepEqual(selection.folders(), ["/data/photos"]);
  assert.deepEqual(selection.skips(), ["/data/photos/secret"]);

  // The engine's FolderSelection promise stands: a pick inside a skipped folder is
  // chosen, chosen folders are, and the indexer receives them all.
  selection.chosen = ["/data/photos", "/data/photos/secret"];
  selection.skipped = ["/data/photos/secret"];
  assert.deepEqual(selection.folders().slice().sort(),
    ["/data/photos", "/data/photos/secret"]);
});
