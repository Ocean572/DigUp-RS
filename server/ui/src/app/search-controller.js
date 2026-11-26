// search-controller.js — keyword-first typing, exactly as the desktop panel types
// (the vanilla app.js logic carried over verbatim): words at once with the last word
// as a prefix, meaning after the pause, the query embedded while you type (embed_ahead),
// strong/weaker grouping, selection bookkeeping. Sequencing tokens (generation,
// finalGeneration, the pause timer) stay HERE — they are sequencing, not app state
// (AGENTS.md §State). The results slice it writes is what digup-results/preview/footer read.
import { KIND_TITLE, WEAKER } from "../lib/format.js";

const TYPING_PAUSE = 120;              // ms after the last keystroke until meaning results

export function createSearchController({ api, store, pause = TYPING_PAUSE }) {
  const seq = { generation: 0, finalGeneration: -1, timer: null };

  const results = () => store.get().results;

  /** "name: …" is the engine's own prefix (`NameQuery`, parsed inside `Searcher.search`), so
   *  a names search rides the query TEXT and the engine keeps its model asleep for it
   *  (`SearchService` checks the prefix before it embeds). Composed once, never doubled:
   *  text the user typed with the prefix in it passes through as typed. */
  const named = (text, isNames) => (isNames && !/^name:/i.test(text) ? "name: " + text : text);

  function searchBody(text, meaning, prefixLast) {
    const s = store.get();
    const f = s.filters;
    const body = {q: named(text, s.isNames), code: s.isCode, meaning, limit: f.limit};
    if (prefixLast) body.prefix_last = true;
    if (!s.isCode) {
      const kinds = f.kinds;
      body.kinds = (kinds.length === 0 || kinds.length === 6) ? null : kinds;   // none ticked = all
      // The People filter (round six, 2026-11-23): the server keeps only hits whose photo
      // carries the person's face (faces-store membership, relayed — the z's ride verbatim).
      if (f.person) body.person = f.person;
      // The scope keeps (#206/#132/#188, 2026-11-25): relayed verbatim — the engine
      // narrows the scope inside its roots and keeps rows after the ranking. Absent
      // fields keep the byte-parity verb for every pre-round pin.
      if (f.under) body.under = f.under;
      if (f.ext) body.ext = f.ext;
    }
    return body;
  }

  /** The ONE search route (vanilla queryChanged): the typed text, the code prefix, the
   *  keyword-first pair of calls, the embed-ahead warm-up, the generation guards. */
  function queryChanged(typed, filters) {
    store.set({ filters });
    const trimmed = typed.trim();
    const prefixCode = /^code:/i.test(trimmed);
    const prefixNames = /^name:/i.test(trimmed);
    const code = prefixCode || filters.code;
    // One prefix per query, the engine's own order ("code:" leads a text that carries both).
    const names = !code && (prefixNames || filters.names === true);
    const text = prefixCode ? trimmed.slice(5).trim() : typed;
    store.set({ isCode: code, isNames: names, text });
    const generation = ++seq.generation;
    clearTimeout(seq.timer);
    store.set({ navigated: false, error: null });
    if (!trimmed || (code && !text)) {
      show({hits: [], strong: 0}, code && !text ? "codeHint" : "empty");
      return;
    }
    // Words now. The last word is probably half-typed, so it matches as a prefix.
    api("/api/v1/search", {method: "POST", body: JSON.stringify(searchBody(text, false, true))})
      .then((reply) => { if (generation === seq.generation && seq.finalGeneration !== generation) show(reply, "words"); })
      .catch((error) => { if (generation === seq.generation) failed(error); });
    // The meaning is embedded now, so its vector is ready when typing pauses (embed_ahead).
    // A names query rides the prefix, so the engine's own check keeps the encoder asleep for it.
    api("/api/v1/embed_ahead", {method: "POST", body: JSON.stringify({q: named(text, names), code})}).catch(() => {});
    seq.timer = setTimeout(() => {
      api("/api/v1/search", {method: "POST", body: JSON.stringify(searchBody(text, true, false))})
        .then((reply) => {
          if (generation !== seq.generation) return;
          seq.finalGeneration = generation;
          show(reply, reply.meaning ? "meaning" : "wordsOnly");
        })
        .catch((error) => { if (generation === seq.generation) failed(error); });
    }, pause);
  }

  function failed(error) {
    store.set({ error: String(error.message || error) });
  }

  /** `find like this` (#162/#121, 2026-11-25): the clicked photo rides as its
   *  minted media handle — the REST law keeps paths out of the API; the server
   *  resolves the handle and the engine embeds the reference (media encoder) and
   *  ranks the pool by cosine: the same 2.0σ gate, the same filter chain, the
   *  same verbatim rows. The current kind/scope/ext filters ride the same fields
   *  as a text search (the People filter does not: find-like answers pictures).
   *  The generation law rides (a late answer never overwrites newer typing), and
   *  the rows land in the SAME results machinery — groups, the weaker fold, the
   *  selection bookkeeping. */
  function findLike(mediaId) {
    if (!mediaId) return;
    const s = store.get();
    const f = s.filters;
    const body = {media_id: mediaId, limit: f.limit};
    const kinds = f.kinds;
    body.kinds = (kinds.length === 0 || kinds.length === 6) ? null : kinds;   // none ticked = all
    if (f.under) body.under = f.under;
    if (f.ext) body.ext = f.ext;
    const generation = ++seq.generation;
    clearTimeout(seq.timer);
    store.set({navigated: false, error: null});
    api("/api/v1/find-like", {method: "POST", body: JSON.stringify(body)})
      .then((reply) => {
        if (generation !== seq.generation) return;
        seq.finalGeneration = generation;
        show(reply, "findLike");
      })
      .catch((error) => { if (generation === seq.generation) failed(error); });
  }

  // The hits: strong results grouped by kind, the rest folded ("N weaker matches").
  function show(reply, phase) {
    const hits = reply.hits || [];
    const strong = Math.min(reply.strong ?? hits.length, hits.length);
    const s = store.get();
    const prev = s.results;
    const strongRows = hits.slice(0, strong);
    const weakerRows = hits.slice(strong);
    const groups = new Map();
    for (const hit of strongRows) {
      const title = KIND_TITLE[hit.kind] || "Files";
      if (!groups.has(title)) groups.set(title, []);
      groups.get(title).push(hit);
    }
    const navigated = s.navigated;
    const next = {
      phase, strong: strongRows, weaker: weakerRows,
      groups: [...groups].map(([title, rows]) => ({title, rows})),
      hitsById: new Map(hits.map((hit) => [hit.media_id, hit])),
      showWeaker: navigated ? prev.showWeaker : false,
      selection: prev.selection, lastReply: reply,
    };
    const ids = rowIDs(next);
    if (!navigated || !ids.includes(next.selection)) next.selection = ids[0] ?? null;
    store.set({ results: next });
  }

  function rowIDs(r = results()) {
    return [
      ...r.groups.flatMap((group) => group.rows.map((hit) => hit.media_id)),
      ...(r.weaker.length ? [WEAKER] : []),
      ...(r.showWeaker ? r.weaker.map((hit) => hit.media_id) : []),
    ];
  }

  function move(step) {
    const ids = rowIDs();
    if (!ids.length) return;
    const s = store.get();
    const at = ids.indexOf(s.results.selection);
    const selection = ids[Math.max(0, Math.min(ids.length - 1, (at < 0 ? -1 : at) + step))];
    store.set({ navigated: true, results: { ...s.results, selection } });
  }

  function select(mediaId) {
    const s = store.get();
    store.set({ navigated: true, results: { ...s.results, selection: mediaId } });
  }

  function toggleWeaker() {
    const s = store.get();
    store.set({ results: { ...s.results, showWeaker: !s.results.showWeaker, selection: WEAKER } });
  }

  const selectedHit = () => { const s = store.get(); return s.results.hitsById.get(s.results.selection) ?? null; };

  return { queryChanged, move, select, toggleWeaker, selectedHit, rowIDs, failed, findLike };
}
