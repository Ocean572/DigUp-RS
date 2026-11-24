// people-controller.js — the People tab (docs/implementation/FACES.md Phase 3): the
// consent record, the scan, the person verbs. Thin-client law: every verb rides
// services/api.js (the bearer header belongs there — the faces surface is admin-guarded
// like the index writer), and every number is the engine's, relayed. The server's consent
// wall answers 400 before any daemon spawn; the panel reads `consented` from /faces/status
// and shows the acceptance act, never a bypass.
//
// The scan is SYNCHRONOUS on the engine's clock (detection is seconds per photo on CPU —
// a big tree is minutes to hours): the panel carries a spinner while the request is in
// flight, and every scan reply is tokened (the estimate's generation rule) — a late
// answer cannot write rows into a panel closed and reopened in between. Closing the
// panel invalidates the in-flight scan (there is no engine cancel — the scan finishes
// server-side behind its writer lock; only the late write is dropped).

/** The slice's blank shape: the panel renders it before any load, and a withdraw resets
 *  to it. The store starts with `people: null` (main.js) — the controller seeds this. */
export const PEOPLE_SLICE = {
  consented: false, record: null, models: [], counts: null,
  persons: [], pending: [], facesFor: null, names: [],
  scan: { spinning: false, result: null, error: null },
  error: null,
};

export function createPeopleController({ api, store, confirm = (m) => window.confirm(m) }) {
  const seq = { scan: 0 };

  function patch(p) {
    store.set({ people: { ...(store.get().people || PEOPLE_SLICE), ...p } });
  }

  /* GET /faces/status: consented + record + the six pins' presence + the engine's counts
     (zeroes before consent — the server answers them itself, no daemon behind the wall). */
  async function loadStatus() {
    try {
      const st = await api("/api/v1/faces/status");
      patch({ consented: st.consented, record: st.record, models: st.models || [],
              counts: st.counts, error: null });
      return st.consented;
    } catch (error) {
      patch({ error: `faces status: ${error.message}` });
      return false;
    }
  }

  /* The two lists + the counts, re-read after any verb: every mutation ends with the
     engine's own answer, never an optimistic client-side edit. */
  async function loadRows() {
    try {
      const [p, q, st] = await Promise.all([
        // `?limit=300` = the grid cap below: the verb pays its one cover query per
        // person (34 s for 8,868 people measured on the live store), and the panel
        // paints at most GRID_CAP cards — ask for exactly the window, count-desc.
        api("/api/v1/faces/persons?limit=300"), api("/api/v1/faces/pending"), api("/api/v1/faces/status"),
      ]);
      patch({ persons: p.persons, pending: q.pending, counts: st.counts,
              consented: st.consented });
    } catch (error) {
      patch({ error: `faces: ${error.message}` });
    }
  }

  async function open() {
    const consented = await loadStatus();
    if (consented) {
      await loadRows();
      loadNames(); // the datalist loads behind the grid — the cards never wait for it
    }
    return consented;
  }

  /* The affirmative act (FACES.md §5: no pre-ticked box): the panel checks the box and
     sends accept:true with the embedder tier the user's select names (omitted → the
     server's default pin). */
  async function accept(embModel) {
    const body = { accept: true };
    if (embModel) body.emb_model = embModel;
    try {
      await api("/api/v1/faces/consent", { method: "POST", body: JSON.stringify(body) });
      await open();
    } catch (error) {
      patch({ error: `consent: ${error.message}` });
    }
  }

  /* Opting out closes the verbs (the store stays — mass deletion is never a side
     effect, the suite pins it): reset the slice, re-read the status. */
  async function withdraw() {
    if (!confirm("Withdraw face-recognition consent? The People verbs close; the face store stays on disk until you delete it.")) return false;
    try {
      await api("/api/v1/faces/consent", { method: "DELETE" });
      store.set({ people: { ...PEOPLE_SLICE } });
      await loadStatus();
      return true;
    } catch (error) {
      patch({ error: `withdraw: ${error.message}` });
      return false;
    }
  }

  /* POST /faces/scan: synchronous on the engine's clock, its own writer lock (busy is
     the server's 409 sentence, relayed). Tokened: a newer scan or a closed panel drops
     this reply. On landing: the scan's counts + a re-read of the lists. */
  async function scanNow() {
    const token = ++seq.scan;
    patch({ scan: { spinning: true, result: null, error: null } });
    try {
      const { scan } = await api("/api/v1/faces/scan", { method: "POST",
                                                         body: JSON.stringify({}) });
      if (token !== seq.scan) return;
      patch({ scan: { spinning: false, result: scan, error: null } });
      await loadRows();
    } catch (error) {
      if (token !== seq.scan) return;
      patch({ scan: { spinning: false, result: null, error: error.message } });
    }
  }

  /* A verb on one person / one queued face: relay, then re-read. The engine's answers
     carry no rows of their own — the lists come from the verbs that list them. */
  async function verb(path, body, label) {
    try {
      await api(`/api/v1/faces/${path}`, { method: "POST", body: JSON.stringify(body) });
      await loadRows();
      return true;
    } catch (error) {
      patch({ error: `${label}: ${error.message}` });
      return false;
    }
  }

  function namePerson(person, name) {
    const text = (name || "").trim();
    if (!text) return Promise.resolve(false);     // an empty rename is not an act
    return verb("name", { person, name: text }, "name");
  }
  function merge(into, from) {
    return verb("merge", { into, from }, "merge");
  }
  function confirmFace(face, person) {
    return verb("confirm", { face, person }, "confirm");
  }

  /* Deleting a person is NOT deleting photos: its face rows land in the review queue
     (the engine's own words, the suite pins the cascade). */
  async function deletePerson(person) {
    if (!confirm(`Delete person #${person}? Their faces move to the review queue — no photo is deleted.`)) return false;
    return verb("delete", { person }, "delete");
  }

  /* A card's face grid: GET /faces/person/{id} as a PAINTED page (2026-11-23): the
     newest photos first (`order=photo` = the photos' own dates), 50 at a time; the
     panel keeps scrolling as the user scrolls (the minted preview handles ride the
     rows — the panel crops them with each row's box/width/height). A mega-cluster
     card (7,294 photos) never loads its whole gallery at once — the pages are the
     UI's law, the engine relays them. */
  const PAGE = 50;
  async function openPerson(person) {
    try {
      const { faces } = await api(
        `/api/v1/faces/person/${person}?limit=${PAGE}&offset=0&order=photo`);
      patch({ facesFor: { person, faces, more: faces.length === PAGE, loading: false } });
    } catch (error) {
      patch({ error: `person ${person}: ${error.message}` });
    }
  }
  async function morePerson(person) {
    const ff = (store.get().people || PEOPLE_SLICE).facesFor;
    if (!ff || ff.person !== person || ff.loading || !ff.more) return;
    patch({ facesFor: { ...ff, loading: true } });
    try {
      const { faces } = await api(
        `/api/v1/faces/person/${person}?limit=${PAGE}&offset=${ff.faces.length}&order=photo`);
      patch({ facesFor: { person: ff.person, faces: [...ff.faces, ...faces],
                           more: faces.length === PAGE, loading: false } });
    } catch (error) {
      patch({ facesFor: { ...ff, loading: false }, error: `person ${person}: ${error.message}` });
    }
  }
  function closePerson() {
    patch({ facesFor: null });
  }
  /* The autocomplete law (2026-11-23): the names the user has given are the merge
     targets — GET /faces/persons?named=1 answers the named people only, count-desc. */
  async function loadNames() {
    try {
      const { persons } = await api("/api/v1/faces/persons?named=1");
      patch({ names: persons.map((p) => ({ id: p.id, name: p.name, count: p.count })) });
    } catch { /* the datalist is a convenience: a failed read keeps the plain name box */ }
  }

  /* Closing the panel: the in-flight scan (if any) is invalidated — its late reply
     finds the token moved and writes nothing, and the spinner clears with the panel
     (a panel reopened tomorrow never shows yesterday's scan as running). */
  function close() {
    seq.scan++;
    const people = store.get().people;
    if (people && people.scan.spinning) {
      store.set({ people: { ...people, scan: { ...PEOPLE_SLICE.scan } } });
    }
  }

  return { open, accept, withdraw, scanNow, namePerson, merge, deletePerson, confirmFace,
           openPerson, closePerson, morePerson, loadNames, close,
           get scanToken() { return seq.scan; } };
}
