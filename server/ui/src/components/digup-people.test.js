// digup-people — the People panel: the consent gate (no pre-ticked box, Accept gated on
// it, tier badges, the honest "packs not installed" line), the scan button + the state
// line (spinner / ✓ summary / ! error), the person cards (CSS-cropped previews, the
// crop map geometry, the width-0 fallback), the rename/merge/delete controls, the
// review queue with the engine's candidate cosines, and the events the shell wires.
// Behavior + geometry, not strings: the crop style values are computed from the row's
// relayed numbers (the scale-invariant law in the component header).
import { describe, expect, it, afterEach } from "vitest";
import "./digup-people.js";
import { cropGeometry } from "../lib/faces.js";
import { createStore } from "../app/store.js";
import { onceEvent } from "../lib/test-util.js";

const MODELS = [
  { name: "scrfd_10g_bnkps.onnx", present: true },
  { name: "w600k_mbf.onnx", present: true },
  { name: "w600k_r50.onnx", present: false },
  { name: "adaface_ir_18.onnx", present: false },
  { name: "adaface_ir_50.onnx", present: false },
  { name: "adaface_ir_101.onnx", present: false },
];
const COVER = { id: 3, path: "/m/wakeboard-logan.jpg", size: 73640, mtime: 1.0,
  box: [10, 10, 60, 60], kps: [[20, 20], [40, 20], [30, 35], [18, 50], [45, 50]],
  score: 0.88, width: 200, height: 100, person: 1, media_id: "aaa" };
// A width-0 row (schema 3's pre-map fallback): the card shows the whole photo.
const PLAIN = { id: 4, path: "/m/fishing-from-dock-oregon.jpg", size: 39627, mtime: 1.0,
  box: [165.9, 220.9, 177.5, 233.0], kps: [[171, 225], [0, 0], [0, 0], [0, 0], [0, 0]],
  score: 0.5, width: 0, height: 0, person: 2, media_id: "bbb" };
const PENDING_FACE = { id: 9, path: "/m/man-on-dock-with-fish-1900.jpg", size: 78052, mtime: 1.0,
  box: [369, 356, 402, 397], kps: [[377, 371], [0, 0], [0, 0], [0, 0], [0, 0]],
  score: 0.83, width: 640, height: 480, person: null, media_id: "ccc" };

const CONSENTED = {
  consented: true, record: { consented: true, accepted_at: 5.0,
                             det_model: "scrfd_10g_bnkps.onnx", emb_model: "w600k_mbf.onnx" },
  models: MODELS,
  counts: { faces: 3, assigned: 3, persons: 2, named: 1, pending: 1,
            models: { det: "scrfd_10g_bnkps.onnx", emb: "w600k_mbf.onnx", dim: 512 } },
  persons: [{ id: 1, name: "Angela", count: 2, cover: 0.1, cover_face: COVER },
            { id: 2, name: null, count: 1, cover: 100, cover_face: PLAIN }],
  pending: [{ face: PENDING_FACE, candidates: [{ person: 1, name: "Angela", cos: 0.42 }] }],
  facesFor: null, scan: { spinning: false, result: null, error: null }, error: null,
};
const CLOSED = { ...CONSENTED, consented: false, record: null, persons: [], pending: [],
                 counts: { faces: 0, assigned: 0, persons: 0, named: 0, pending: 0 } };

async function mount(over = {}) {
  const store = createStore({ people: { ...CLOSED, ...over } });
  const el = document.createElement("digup-people");
  el.store = store;
  document.body.append(el);
  await el.updateComplete;
  return el;
}

describe("digup-people", () => {
  let el;
  afterEach(() => el?.remove());

  it("the gate: an enable screen, the Accept disabled by the unchecked box, the tier badges", async () => {
    el = await mount();
    expect(el.shadowRoot.textContent).toContain("Face recognition is off");
    const accept = el.shadowRoot.getElementById("consent-accept");
    expect(accept.disabled).toBe(true);                    // no pre-ticked box, no implicit yes
    const cards = el.shadowRoot.querySelector("#people-grid");
    expect(cards).toBe(null);                              // closed: no cards, no scan button
    expect(el.shadowRoot.getElementById("faces-scan")).toBe(null);
    const badges = [...el.shadowRoot.querySelectorAll(".set-sec .badge")].map((b) => b.textContent);
    expect(badges).toContain("w600k_mbf.onnx");
    expect(badges.length).toBe(MODELS.length);             // all six pins, presence-stamped
  });

  it("checking the box enables Accept; Accept emits the consent act with the named tier", async () => {
    el = await mount();
    const accept = el.shadowRoot.getElementById("consent-accept");
    el.shadowRoot.getElementById("consent-ack").click();  // native checkbox: click fires change
    await el.updateComplete;
    expect(accept.disabled).toBe(false);
    const seen = onceEvent(el, "digup:consent");
    accept.click();
    const e = await seen;
    expect(e.detail.embModel).toBe("w600k_mbf.onnx");     // the select's default pin
  });

  it("an install without the pinned packs says so and keeps Accept dead", async () => {
    el = await mount({ models: MODELS.map((m) => ({ ...m, present: false })) });
    el.shadowRoot.getElementById("consent-ack").click();
    await el.updateComplete;
    expect(el.shadowRoot.getElementById("consent-accept").disabled).toBe(true);
    expect(el.shadowRoot.textContent).toContain("download-model --faces");
  });

  it("consented: counts, the scan control, the withdraw line", async () => {
    el = await mount(CONSENTED);
    expect(el.shadowRoot.getElementById("faces-scan").textContent.trim()).toBe("Scan now");
    const badges = [...el.shadowRoot.querySelectorAll(".set-line .badge")].map((b) => b.textContent);
    expect(badges).toContain("3 faces");
    expect(badges).toContain("2 people");
    expect(badges).toContain("1 to review");
    expect(el.shadowRoot.getElementById("faces-withdraw")).not.toBe(null);
  });

  it("the scan state line: spinner while in flight, ✓ + the engine's counts after", async () => {
    el = await mount({ ...CONSENTED, scan: { spinning: true, result: null, error: null } });
    expect(el.shadowRoot.querySelector(".estimate-spin")).not.toBe(null);
    expect(el.shadowRoot.getElementById("faces-scan").disabled).toBe(true);
    el.remove();
    el = await mount({ ...CONSENTED, scan: { spinning: false, result: { files: 12, faces: 3, skipped: 0, fresh: 12, assigned: 3 }, error: null } });
    expect(el.shadowRoot.getElementById("people-state").textContent)
      .toContain("12 files · 3 faces · 12 fresh");
  });

  it("cards crop the cover face with the relayed map — the scale cancels, the map does not", async () => {
    el = await mount(CONSENTED);
    const cards = [...el.shadowRoot.querySelectorAll("#people-grid .person-card")];
    expect(cards.length).toBe(2);
    const crop = cards[0].querySelector(".crop");
    expect(crop.querySelector("img").getAttribute("src")).toBe("/api/v1/media/aaa/preview?pixels=512");
    // box [10,10,60,60] in 200×100, padded head-and-shoulders and clamped: the window
    // is cw 82.5 × ch 100 (the shoulders padding hits the photo's bottom edge), the
    // photo enlarged by W/cw, offsets 0 (pure geometry, pinned on the numbers — jsdom's
    // CSS parser cannot see the inline styles, the browser can).
    expect(cropGeometry(COVER)).toEqual({ cw: 82.5, ch: 100, cx0: 0, cy0: 0, W: 200 });
    expect(cropGeometry(PLAIN)).toBe(null);                // width 0: the whole-photo fallback
    expect(cards[1].querySelector("img.plain")).not.toBe(null);
    expect(cards[1].querySelector("img.plain").getAttribute("src"))
      .toBe("/api/v1/media/bbb/preview?pixels=512");
  });

  it("the card controls: the name rides the rename, the merge strip is ONE strip (2 selects, not N² per card), Delete emits", async () => {
    el = await mount(CONSENTED);
    const [card1, card2] = [...el.shadowRoot.querySelectorAll("#people-grid .person-card")];
    expect(card1.querySelector(".person-name").value).toBe("Angela");
    expect(card2.querySelector(".person-name").getAttribute("placeholder")).toBe("Person 2");
    // Finding-1 cure (2026-11-23): NO merge select in any card — the picker is
    // one strip per panel: two selects = 2N options, never N² (the 37k-card
    // panel answered "Nothing found yet" at 1.4 billion option nodes).
    expect(card1.querySelector("select")).toBe(null);
    const strip = el.shadowRoot.querySelector(".merge-strip");
    const from = strip.querySelector(".merge-from");
    const into = strip.querySelector(".merge-into");
    expect([...from.querySelectorAll("option")].map((o) => o.textContent))
      .toEqual(["Merge this person…", "Angela (2)", "Person 2 (1)"]);
    expect(into.disabled).toBe(true);                          // a source must be chosen first
    let seen = onceEvent(el, "digup:name");
    card1.querySelector(".person-name").value = "Angela M";
    card1.querySelector(".person-name").dispatchEvent(new Event("change"));
    expect((await seen).detail).toEqual({ person: 1, name: "Angela M" });
    from.value = "1";
    from.dispatchEvent(new Event("change"));
    await el.updateComplete;                                     // Lit state re-render is a microtask
    expect([...into.querySelectorAll("option")].map((o) => o.textContent))
      .toEqual(["into this person…", "Person 2 (1)"]);         // the target excludes the source
    expect(into.disabled).toBe(false);
    seen = onceEvent(el, "digup:merge");
    into.value = "2";
    into.dispatchEvent(new Event("change"));
    await el.updateComplete;                                     // the Merge button wakes
    strip.querySelector(".merge-go").click();
    expect((await seen).detail).toEqual({ into: 2, from: 1 });
    seen = onceEvent(el, "digup:delete");
    card2.querySelector(".person-delete").click();
    expect((await seen).detail).toEqual({ person: 2 });
  });

  it("a card crop opens the face grid (digup:open-person)", async () => {
    el = await mount(CONSENTED);
    const seen = onceEvent(el, "digup:open-person");
    el.shadowRoot.querySelector("#people-grid .person-card .crop").click();
    expect((await seen).detail).toEqual({ person: 1 });
  });

  it("the review queue: the face, its file, the engine's candidates, confirm by number", async () => {
    el = await mount(CONSENTED);
    const sec = el.shadowRoot.getElementById("pending-sec");
    expect(sec.hasAttribute("hidden")).toBe(false);
    expect(sec.querySelector(".pending-file").textContent).toBe("man-on-dock-with-fish-1900.jpg");
    const cand = sec.querySelector(".pending-cands button");
    expect(cand.textContent.replace(/\s+/g, " ")).toContain("Angela · 0.42");
    const seen = onceEvent(el, "digup:confirm");
    cand.click();
    expect((await seen).detail).toEqual({ face: 9, person: 1 });
    el.remove();
    el = await mount({ ...CONSENTED, pending: [] });
    expect(el.shadowRoot.getElementById("pending-sec").hasAttribute("hidden")).toBe(true);
  });

  it("✕ closes: the shell's close path rides digup:close", async () => {
    el = await mount(CONSENTED);
    const seen = onceEvent(el, "digup:close");
    el.shadowRoot.getElementById("people-close").click();
    expect((await seen)).not.toBe(null);
  });
});
