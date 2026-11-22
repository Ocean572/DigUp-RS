// people-controller — the People tab's sequencing (the store slice as its single writer):
// the consent act and its shape, the scan's generation token (the estimate's rule — a
// late reply cannot write into a closed/reopened panel, and closing invalidates the
// in-flight scan), the verb bodies (person/face names — never `id`, the protocol's reply
// key), the re-read after every mutation (the engine's answer, never an optimistic
// client-side edit), and the honest error relay (api.js's message, verbatim).
import { describe, expect, it, afterEach } from "vitest";
import { createPeopleController } from "./people-controller.js";
import { createStore } from "./store.js";
import { installFetch, mockReply } from "../lib/test-util.js";

const MODELS = [
  { name: "scrfd_10g_bnkps.onnx", present: true },
  { name: "w600k_mbf.onnx", present: true },
  { name: "w600k_r50.onnx", present: false },
  { name: "adaface_ir_18.onnx", present: false },
  { name: "adaface_ir_50.onnx", present: false },
  { name: "adaface_ir_101.onnx", present: false },
];
const COUNTS = { faces: 3, assigned: 3, persons: 2, named: 1, pending: 1,
                 models: { det: "scrfd_10g_bnkps.onnx", emb: "w600k_mbf.onnx", dim: 512 } };
const PERSONS = [{ id: 1, name: "Angela", count: 2, cover: 0.1,
                   cover_face: { id: 3, path: "/m/a.jpg", size: 10, mtime: 1.0,
                                 box: [10, 10, 60, 60], kps: [[1, 2], [3, 4], [5, 6], [7, 8], [9, 10]],
                                 score: 0.8, width: 200, height: 100, person: 1, media_id: "aaa" } }];
const PENDING = [{ face: { id: 9, path: "/m/b.jpg", size: 10, mtime: 1.0,
                           box: [0, 0, 30, 40], kps: [[1, 2], [3, 4], [5, 6], [7, 8], [9, 10]],
                           score: 0.6, width: 0, height: 0, person: null, media_id: "bbb" },
                   candidates: [{ person: 1, name: "Angela", cos: 0.42 }] }];

function routes(over = {}) {
  return {
    "/api/v1/faces/status": mockReply(200, { ok: true, consented: true,
      record: { consented: true, accepted_at: 5.0, det_model: "scrfd_10g_bnkps.onnx", emb_model: "w600k_mbf.onnx" },
      models: MODELS, counts: COUNTS }),
    "/api/v1/faces/persons": mockReply(200, { ok: true, persons: PERSONS }),
    "/api/v1/faces/pending": mockReply(200, { ok: true, pending: PENDING, count: 1 }),
    "POST /api/v1/faces/consent": (p, o) => mockReply(200, { ok: true, consented: true,
      record: { consented: true, ...JSON.parse(o.body), accepted_at: 5.0 } }),
    "POST /api/v1/faces/scan": mockReply(200, { ok: true,
      scan: { files: 12, faces: 3, skipped: 0, fresh: 12, assigned: 3 } }),
    "POST /api/v1/faces/name": mockReply(200, { ok: true }),
    "POST /api/v1/faces/merge": mockReply(200, { ok: true }),
    "POST /api/v1/faces/delete": mockReply(200, { ok: true }),
    "POST /api/v1/faces/confirm": mockReply(200, { ok: true }),
    "DELETE /api/v1/faces/consent": mockReply(200, { ok: true, consented: false, removed: true }),
    "/api/v1/faces/person/1": mockReply(200, { ok: true, faces: PENDING[0].face }),
    ...over,
  };
}

function mount(over, opts = {}) {
  const store = createStore({ people: null });
  const calls = installFetch(routes(over));
  const ctl = createPeopleController({ api: fetchApi(), store, confirm: () => true, ...opts });
  return { store, calls, ctl };
}

// The api() seam of services/api.js, reduced for the controller's contract: JSON body,
// detail-list/plain relay, {status} on the thrown Error (the real client's shape).
function fetchApi() {
  return async (path, options = {}) => {
    const reply = await globalThis.fetch(path, options);
    const body = await reply.json().catch(() => ({}));
    if (!reply.ok) throw Object.assign(new Error(body.detail || reply.statusText), { status: reply.status });
    return body;
  };
}

describe("people-controller", () => {
  afterEach(() => { delete globalThis.fetch; });

  it("open() seeds the slice from /faces/status and does NOT list rows behind the wall", async () => {
    const { store, calls, ctl } = mount({
      "/api/v1/faces/status": mockReply(200, { ok: true, consented: false, record: null,
                                               models: MODELS, counts: { faces: 0, assigned: 0, persons: 0, named: 0, pending: 0 } }),
    });
    await ctl.open();
    const p = store.get().people;
    expect(p.consented).toBe(false);
    expect(p.counts.faces).toBe(0);
    expect(p.persons).toEqual([]);
    expect(calls.map((c) => c.path)).not.toContain("/api/v1/faces/persons");
  });

  it("open() on a consented deployment reads status + persons + pending", async () => {
    const { store, calls, ctl } = mount();
    expect(await ctl.open()).toBe(true);
    const paths = calls.map((c) => c.path);
    expect(paths).toContain("/api/v1/faces/persons");
    expect(paths).toContain("/api/v1/faces/pending");
    const p = store.get().people;
    expect(p.persons).toEqual(PERSONS);
    expect(p.pending).toEqual(PENDING);
    expect(p.counts).toEqual(COUNTS);
  });

  it("accept() sends the affirmative act, naming the tier only when the UI names one", async () => {
    const { calls, ctl } = mount();
    await ctl.accept();                                   // default: server's pin, no field
    expect(JSON.parse(calls.find((c) => c.options.method === "POST"
      && c.path === "/api/v1/faces/consent").options.body)).toEqual({ accept: true });
    await ctl.accept("w600k_r50.onnx");
    expect(JSON.parse(calls.filter((c) => c.path === "/api/v1/faces/consent"
      && c.options.method === "POST").at(-1).options.body))
      .toEqual({ accept: true, emb_model: "w600k_r50.onnx" });
  });

  it("the scan: spinning while in flight, the engine's counts after, lists re-read", async () => {
    const { store, calls, ctl } = mount({
      "POST /api/v1/faces/scan": () => new Promise((resolve) =>
        setTimeout(() => resolve(mockReply(200, { ok: true,
          scan: { files: 12, faces: 3, skipped: 0, fresh: 0, assigned: 3 } })), 10)),
    });
    const inFlight = ctl.scanNow();
    expect(store.get().people.scan.spinning).toBe(true);
    await inFlight;
    const p = store.get().people;
    expect(p.scan).toEqual({ spinning: false,
                             result: { files: 12, faces: 3, skipped: 0, fresh: 0, assigned: 3 },
                             error: null });
    expect(calls.filter((c) => c.path === "/api/v1/faces/persons").length).toBe(1);
  });

  it("the scan token: a late first answer writes nothing when a newer scan started, and close invalidates the in-flight one", async () => {
    const resolvers = [];
    const { store, ctl } = mount({
      "POST /api/v1/faces/scan": () => {
        const files = (resolvers.length + 1) * 100;
        return new Promise((resolve) => resolvers.push(() =>
          resolve(mockReply(200, { ok: true,
            scan: { files, faces: 1, skipped: 0, fresh: 0, assigned: 1 } }))));
      },
    });
    const slow = ctl.scanNow();                            // resolver #1: files 100
    const fast = ctl.scanNow();                            // resolver #2: files 200
    resolvers[0]();                                        // the SLOW reply resolves first…
    resolvers[1]();                                        // …its token is stale: dropped
    await Promise.all([slow, fast]);
    expect(store.get().people.scan.result.files).toBe(200);
    // Closing the panel invalidates: a scan in flight at close never writes.
    const late = ctl.scanNow();                            // resolver #3 in flight
    ctl.close();
    resolvers[2]();
    await late;
    expect(store.get().people.scan.result).toBe(null);     // close cleared the state
    expect(store.get().people.scan.spinning).toBe(false);  // the late reply wrote nothing
  });

  it("the verbs relay the protocol bodies — person/face names, never `id`", async () => {
    const { calls, ctl } = mount();
    await ctl.namePerson(1, "  Angela  ");
    await ctl.merge(1, 2);
    await ctl.deletePerson(3);
    await ctl.confirmFace(9, 1);
    const bodies = calls.filter((c) => c.options.method === "POST"
                                  && c.path.startsWith("/api/v1/faces/")
                                  && !["/api/v1/faces/consent", "/api/v1/faces/scan"].includes(c.path))
      .map((c) => [c.path, JSON.parse(c.options.body)]);
    expect(bodies).toEqual([
      ["/api/v1/faces/name", { person: 1, name: "Angela" }],
      ["/api/v1/faces/merge", { into: 1, from: 2 }],
      ["/api/v1/faces/delete", { person: 3 }],
      ["/api/v1/faces/confirm", { face: 9, person: 1 }],
    ]);
  });

  it("an empty rename is not an act; a refused delete is not sent", async () => {
    const { calls, ctl } = mount();
    expect(await ctl.namePerson(1, "   ")).toBe(false);
    const denied = createPeopleController({ api: fetchApi(), store: createStore({ people: null }),
                                            confirm: () => false });
    expect(await denied.deletePerson(1)).toBe(false);
    expect(calls.filter((c) => c.path === "/api/v1/faces/delete").length).toBe(0);
  });

  it("openPerson() loads the face grid; closePerson() drops it", async () => {
    const { store, ctl } = mount();
    await ctl.openPerson(1);
    expect(store.get().people.facesFor).toEqual({ person: 1, faces: PENDING[0].face });
    ctl.closePerson();
    expect(store.get().people.facesFor).toBe(null);
  });

  it("errors ride the api's words: scan keeps spinning false with the message, consent keeps the wall", async () => {
    const { store, ctl } = mount({
      "POST /api/v1/faces/scan": mockReply(503, { detail: "faces: no model w600k_mbf.onnx" }),
    });
    await ctl.scanNow();
    expect(store.get().people.scan).toEqual({ spinning: false, result: null,
                                              error: "faces: no model w600k_mbf.onnx" });
  });

  it("withdraw resets the slice and re-reads the wall", async () => {
    const { store, calls, ctl } = mount({
      "/api/v1/faces/status": mockReply(200, { ok: true, consented: false, record: null,
                                               models: MODELS,
                                               counts: { faces: 3, assigned: 3, persons: 2, named: 1, pending: 1 } }),
    });
    await ctl.open();
    expect(await ctl.withdraw()).toBe(true);
    const p = store.get().people;
    expect(p.consented).toBe(false);
    expect(p.persons).toEqual([]);
    expect(p.counts.faces).toBe(3);                       // the store stayed: counts ride the wall's answer
    expect(calls.map((c) => c.options?.method)).toContain("DELETE");
  });
});
