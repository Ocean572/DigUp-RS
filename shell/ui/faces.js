// faces.js — the People tab (Settings → People, docs/implementation/FACES.md): the consent gate, the scan,
// the person cards, the review queue. It is the shell port of the server's own panel
// (server/ui/src/components/digup-people.js + app/people-controller.js), riding the settings page's seams.
// Thin-client law: every number is the engine's, relayed — the counts verb, the scan reply, the crop
// geometry (box/width/height rows mapped onto the helper's own `/render` previews, the server's crop law:
// /render scales the photo by pixels/max(W,H), the box rides source pixels, so the ratios cancel the
// scale). The consent record is kept and enforced by the app and the engine (faces.rs): the gate shows
// ONLY the enable screen — no counts, no cards, no scan button — until the acceptance is recorded.
// The file loads BEFORE settings.js (which owns `tauri`/`invoke`/`$`): the bridge rides window.__TAURI__
// inline, the shortcut.js way, and the settings helpers (element/fail/bytes) are reached at call time.
"use strict";

const DETECTOR = "scrfd_10g_bnkps.onnx";
// The panel paints at most this many person cards (finding-1 cure 2026-11-23,
// measured on the bench library: a full 37k-card grid is unpaintable — the
// cap is on PAINT, the store keeps every person; most-photo people first).
const GRID_CAP = 300;
const PAGE = 50;                    // the gallery's page (the user's 2026-11-23 law: ~ten-to-fifty
                                    // photos at a time, newest photo first, scroll loads more)
const fInvoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const fAsk = (request) => fInvoke("ask", { request });
const fURL = (path) => window.__TAURI__.core.convertFileSrc(path);

/// The panel's whole state, mirroring the server controller's PEOPLE_SLICE: rendered before any
/// load, reset by a withdraw, kept across tab switches (a scan in flight keeps running — closing
/// the tab never cancels the engine's scan, the writer lock law).
const People = {
  consented: false, record: null, models: [], counts: null,
  persons: [], pending: [],
  scan: null,                       // the last finished scan's {files,faces,skipped,fresh,assigned}
  scanning: false, progress: null,  // the engine's scan events, relayed as they stream
  ack: false, embModel: null, download: null,
  openId: null, facesFor: null,
  detail: null,                     // the person whose gallery fills the panel (round-six view)
  names: [],                        // the named people (autocomplete/merge targets, ?named=1 law)
  withdrawing: false, deleting: null,
  error: null,
};

// MARK: crop geometry (the server's lib/faces.js math, pinned by shell/tests/faces.test.cjs)

/// Head-and-shoulders padding over the engine's box, clamped to the photo's own pixels. A row with
/// no map (width 0 — recorded before schema 3) has no window: null, the whole-photo fallback.
function cropGeometry(row) {
  const [x0, y0, x1, y1] = row.box;
  const W = row.width, H = row.height;
  if (!(W > 0 && H > 0) || !(x1 > x0) || !(y1 > y0)) return null;
  const bw = x1 - x0, bh = y1 - y0;
  const cx0 = Math.max(0, x0 - bw * 0.45), cx1 = Math.min(W, x1 + bw * 0.45);
  const cy0 = Math.max(0, y0 - bh * 0.6), cy1 = Math.min(H, y1 + bh * 0.8);
  const cw = Math.max(cx1 - cx0, 1), ch = Math.max(cy1 - cy0, 1);
  return { cw, ch, cx0, cy0, W };
}

// MARK: previews (the results.js render seam, its own cache: the settings page loads no results.js)

const faceRenders = new Map();

/// The helper's own JPEG for a photo, as an asset URL (`ask op:"render"` → reply.image →
/// convertFileSrc — the exact chain panel.js/window.js use; the previews folder is scoped by the
/// search helper's `ready` event, main.rs).
function renderedFace(path, pixels) {
  const key = `${path}|${pixels}`;
  if (!faceRenders.has(key)) {
    faceRenders.set(key, fAsk({ op: "render", path, kind: "image", loc: null, pixels }).then((reply) => fURL(reply.image)));
  }
  return faceRenders.get(key);
}

/// The face crop: a cw/ch window over the enlarged photo (the server's faceCrop law, DOM-built).
/// 512 pixels (2026-11-23, the user's sharpness brief): the card crop is the face you judge.
function faceCrop(row, pixels = 512) {
  const box = element("span", "crop");
  const geometry = cropGeometry(row);
  renderedFace(row.path, pixels).then((url) => {
    const img = element("img");
    img.src = url;
    img.loading = "lazy";
    img.alt = "";
    if (!geometry) {                       // no map (width 0): the whole-photo fallback
      img.className = "plain";
      box.append(img);
      return;
    }
    const { cw, ch, cx0, cy0, W } = geometry;
    box.style.aspectRatio = `${cw} / ${ch}`;
    img.style.width = `calc(100% * ${W / cw})`;
    img.style.left = `calc(-100% * ${cx0 / cw})`;
    img.style.top = `calc(-100% * ${cy0 / ch})`;
    box.append(img);
  }).catch(() => { box.title = "the helper rendered no preview for this photo"; });
  return box;
}

// MARK: relay (faces_status / the verbs; every answer re-renders from the engine's rows)

const personLabel = (p) => p.name || `Person ${p.id}`;

async function peopleLoad() {
  try {
    const st = await fInvoke("faces_status");
    Object.assign(People, { consented: st.consented, record: st.record, models: st.models || [], counts: st.counts, error: null });
    peopleRender();
    if (st.consented) await peopleRows();
  } catch (error) {
    People.error = String(error);
    peopleRender();
  }
}

/// The two lists + the counts, re-read after any verb: every mutation ends with the engine's own
/// answer, never an optimistic client-side edit (the server controller's rule, restated shell-side).
async function peopleRows() {
  try {
    // GRID_CAP window (2026-11-23): the verb pays its cover query per returned row —
    // the whole store costs minutes on a big library (34 s measured for 8,868 people,
    // over the 120 s read clock), and the panel paints at most GRID_CAP cards anyway.
    const [p, q, st] = await Promise.all([fInvoke("faces_persons", { limit: GRID_CAP }), fInvoke("faces_pending"), fInvoke("faces_status")]);
    Object.assign(People, { persons: p.persons || [], pending: q.pending || [], counts: st.counts, consented: st.consented });
    peopleRender();
    loadNames(); // the datalist loads behind the grid — the cards never wait for it
  } catch (error) {
    People.error = String(error);
    peopleRender();
  }
}

async function peopleVerb(command, args, label) {
  try {
    await fInvoke(command, args);
    await peopleRows();
    return true;
  } catch (error) {
    People.error = `${label}: ${error}`;
    peopleRender();
    return false;
  }
}

/// The autocomplete law (2026-11-23): the names the user has given are the merge targets —
/// faces_persons with named:true answers the named people only, photo count first. Cheap:
/// the named set is small, and covers are paid for its rows only.
async function loadNames() {
  try {
    const { persons } = await fInvoke("faces_persons", { named: true });
    People.names = (persons || []).map((p) => ({ id: p.id, name: p.name, count: p.count }));
  } catch { /* the datalist is a convenience: a failed read keeps the plain name box */ }
}

// MARK: the acts

async function peopleAccept() {
  try {
    await fInvoke("faces_consent", { accept: true, embModel: People.embModel });
    await peopleLoad();
  } catch (error) {
    People.error = `consent: ${error}`;
    peopleRender();
  }
}

async function peopleWithdraw() {
  People.withdrawing = false;
  try {
    await fInvoke("faces_consent", { accept: false });
    Object.assign(People, { consented: false, record: null, counts: null, persons: [], pending: [], names: [], scan: null, facesFor: null, openId: null, detail: null });
    await peopleLoad();
  } catch (error) {
    People.error = `withdraw: ${error}`;
    peopleRender();
  }
}

/// The scan: one press runs the WHOLE folder in one session (2026-11-22, the user's law — the
/// shell's scan clock is a 30-day watchdog, not a bound; detection is seconds per photo on CPU,
/// so a big tree is hours, and hours is what one session does). Files already recorded are
/// skipped, so a closed app resumes exactly where it stopped on the next press. There is no
/// cancel: closing the tab never stops the engine.
/// The crawl lines the engine relays are CUMULATIVE per root (crawler::crawl progress,
/// face_service.rs) — summing them inflates quadratically (a 132k-file tree painted 300 M
/// "checked"). Keep the latest line per root; the shown total is their sum, reset per scan.
let crawlRoots = {};

async function peopleScan() {
  if (People.scanning) return;
  People.scanning = true;
  People.scan = null;
  crawlRoots = {};
  People.progress = { checked: 0, faces: 0, last: "", cluster: null };
  peopleRender();
  try {
    const { scan } = await fInvoke("faces_scan", {});
    People.scan = scan;
    await peopleRows();
  } catch (error) {
    People.scan = { error: String(error) };
  }
  People.scanning = false;
  People.progress = null;
  peopleRender();
}

async function peopleOpenPerson(person) {
  People.openId = person;
  People.detail = person;          // round-six (the user's law): the gallery FILLS the view,
  peopleRender();                  // the grid is not squeezed by an inline strip
  try {
    // The painted page (2026-11-23): newest photos first (`order:"photo"` = the photos'
    // own dates), PAGE at a time. A mega-cluster card (7,294 photos) never reads its
    // whole gallery at once — the pages are the UI's law, the engine relays them.
    const { faces } = await fInvoke("faces_person", { person, limit: PAGE, offset: 0, order: "photo" });
    People.facesFor = { person, faces: faces || [],
                        more: (faces || []).length === PAGE, loading: false };
    peopleRender();
  } catch (error) {
    People.facesFor = null;
    People.detail = null;
    People.error = `person ${person}: ${error}`;
    peopleRender();
  }
}

/// The gallery scroll: the next PAGE, appended in the engine's photo-date order. A short
/// page means the store is drained — the button disappears with it (no invented counts).
async function peopleMorePerson(person) {
  const ff = People.facesFor;
  if (!ff || ff.person !== person || ff.loading || !ff.more) return;
  People.facesFor = { ...ff, loading: true };
  peopleRender();
  try {
    const { faces } = await fInvoke("faces_person", { person, limit: PAGE, offset: ff.faces.length, order: "photo" });
    const rows = faces || [];
    People.facesFor = { person, faces: [...ff.faces, ...rows],
                        more: rows.length === PAGE, loading: false };
  } catch (error) {
    People.facesFor = { ...ff, loading: false };
    People.error = `person ${person}: ${error}`;
  }
  peopleRender();
}

async function peopleDownload() {
  $("problem").hidden = true;
  People.download = { state: "checking" };
  peopleRender();
  try {
    await fInvoke("faces_download");
  } catch (error) {
    People.download = { state: "failed", message: String(error) };
    peopleRender();
  }
}

/// The gallery's infinite scroll (round six, the user's law: keep scrolling, load as you go,
/// nothing loads until you scroll): the sentinel at the gallery's foot, watched by the page's
/// own viewport. A hit fires the engine's next page — relayed, never prefetched.
let galleryObserver = null;

function watchGallerySentinel(sentinel, person) {
  if (galleryObserver) galleryObserver.disconnect();
  galleryObserver = new IntersectionObserver((entries) => {
    for (const entry of entries) if (entry.isIntersecting) peopleMorePerson(person);
  }, { rootMargin: "600px 0px" });
  galleryObserver.observe(sentinel);
}

/// The card's name box (the autocomplete law, 2026-11-23): datalist over the store's named
/// people — picking a known name is the human's merge act (merge this person INTO the one
/// carrying that name); a fresh name is the plain name act. The panel relays; the engine merges.
function peopleNameBox(person) {
  const name = element("input", "field person-name");
  name.type = "text";
  name.value = person.name || "";
  name.placeholder = `Person ${person.id}`;
  name.setAttribute("aria-label", `Name for person ${person.id}`);
  const list = element("datalist");
  list.id = `names-${person.id}`;
  for (const n of People.names) {
    if (n.id === person.id || !n.name) continue;
    const opt = element("option");
    opt.value = n.name;
    opt.label = `${n.name} (${n.count})`;
    list.append(opt);
  }
  name.setAttribute("list", list.id);
  name.addEventListener("change", () => {
    const text = name.value.trim();
    const target = People.names.find((n) => n.name === text && n.id !== person.id);
    if (target) peopleVerb("faces_merge", { into: target.id, from: person.id }, "merge");
    else peopleVerb("faces_name", { person: person.id, name: name.value }, "name");
  });
  return { name, list };
}

/// The person's OWN view (round six, the user's brief): the gallery fills the panel —
/// newest photo first, 50 at a time, the page scrolls, the next page loads when you reach
/// the foot. The grid's rows are never squeezed by an inline strip.
function peopleDetailView(person) {
  const view = element("div", "people-detail");
  const head = element("div", "detail-head");
  const back = element("button", "", "← People");
  back.title = "Back to the people grid";
  back.onclick = () => {
    People.detail = null; People.facesFor = null; People.openId = null;
    if (galleryObserver) galleryObserver.disconnect();
    peopleRender();
  };
  head.append(back, element("h3", "", personLabel(person)));
  const badges = element("span", "people-facts");
  badges.append(peopleBadge(`${person.count} photo${person.count === 1 ? "" : "s"}`, person.count > 1));
  head.append(badges);
  const nameBox = peopleNameBox(person);
  head.append(nameBox.name, nameBox.list);
  view.append(head);
  const grid = element("div", "detail-grid");
  for (const face of People.facesFor.faces) grid.append(faceCrop(face, 512));
  view.append(grid);
  if (People.facesFor.loading) view.append(element("p", "note", "Loading the next photos…"));
  else if (People.facesFor.more) {
    view.append(element("p", "note",
      `Keep scrolling — the next ${PAGE} photos load as you go (${People.facesFor.faces.length.toLocaleString()} of ${person.count.toLocaleString()} shown).`));
    const sentinel = element("div", "detail-sentinel");
    view.append(sentinel);
    if (typeof IntersectionObserver !== "undefined") {
      watchGallerySentinel(sentinel, person.id);
    } else {
      const more = element("button", "face-more", "Show 50 more");
      more.onclick = () => peopleMorePerson(person.id);
      view.append(more);
    }
  } else {
    view.append(element("p", "note",
      `All ${People.facesFor.faces.length.toLocaleString()} photos shown — newest first.`));
  }
  return view;
}

// MARK: events (the engine's scan stream and the packs' download, relayed)

window.__TAURI__.event.listen("faces", ({ payload }) => {
  if (!People.scanning || !People.progress) return;
  const event = payload.event || {};
  if (event.event === "crawl") {
    crawlRoots[event.root] = event.checked || 0;
    People.progress.checked = Object.values(crawlRoots).reduce((a, b) => a + b, 0);
  } else if (event.event === "face-scan") {
    People.progress.faces += event.faces || 0;
    People.progress.last = event.path || People.progress.last;
  } else if (event.event === "cluster") People.progress.cluster = event;
  peopleProgressLine();
});

window.__TAURI__.event.listen("faces-model", ({ payload }) => {
  const event = payload.event || payload;
  const kind = event.event || "";
  if (kind === "progress") People.download = { state: "downloading", bytes: event.all_bytes || 0, total: event.all_total || 1 };
  else if (kind === "file") People.download = { state: "downloading", from: event.from };
  else if (kind === "verifying") People.download = { state: "checking" };
  else if (kind === "waiting") People.download = { state: "waiting" };
  else if (kind === "done") { People.download = { state: "ready" }; peopleLoad(); return; }
  else if (kind === "error") People.download = { state: "failed", message: event.message };
  else if (kind === "stopped") People.download = { state: "failed", message: "the download stopped" };
  else return;
  peopleDownloadLine();
});

// MARK: render

function peopleRender() {
  const host = $("people-panel");
  if (!host) return;
  host.replaceChildren(People.consented ? peopleConsentedView() : peopleGateView());
}

function peopleBadge(text, present) {
  return element("span", `people-badge ${present ? "ok" : "off"}`, text);
}

function peopleFacts(models) {
  const facts = element("span", "people-facts");
  facts.replaceChildren(...models.map((m) => {
    const badge = element("span", `people-badge ${m.present ? "ok" : "off"}`, m.name);
    badge.title = m.present ? "installed in the models folder" : "not installed";
    return badge;
  }));
  return facts;
}

/// The enable screen (FACES.md §5, the default-OFF posture): no counts, no cards, no scan.
function peopleGateView() {
  const view = element("div", "people-view");
  // The consent explanation (round eight, the user's ask 2026-11-25): the gate explains WHY
  // the packs download and WHAT acceptance authorizes, in plain words, BEFORE the checkbox.
  // Thin-client law keeps it honest: it states what the engine does, promises nothing more.
  view.append(element("h3", "", "Face recognition is off — what switching it on does"));
  view.append(element("p", "note people-copy",
    "Face recognition runs on two model packs: a detector that finds faces, and an embedder "
    + "that turns each face into numbers. They are plain files DigUp downloads once from "
    + "Hugging Face into its models folder. That download is the only network step: face "
    + "recognition then runs entirely on this computer. Photos are never uploaded; what a "
    + "face becomes — numbers and crops — is stored only in DigUp's own face store beside "
    + "its index. No account, no cloud."));
  view.append(element("p", "note people-copy",
    "Accepting records your permission, as a file beside DigUp's settings: DigUp may find "
    + "faces in the photos of the folders you search, describe each face with the tier "
    + "chosen below, and group matching faces into people you can name. You can withdraw at "
    + "any time — scanning stops, and withdrawing never deletes photos."));
  view.append(peopleFacts(People.models));
  const tiers = People.models.filter((m) => m.name !== DETECTOR && m.present);
  if (tiers.length) {
    const select = element("select", "field people-emb");
    select.title = "The embedder tier the record names — one per face store";
    // The pick is stored (People.embModel) and the option's `selected` follows it: the settings
    // page re-renders the gate on focus (which is exactly when a native dropdown closes), and a
    // hardcoded `selected` on the default would paint the pick back to w600k_mbf on every rebuild.
    if (People.embModel && !tiers.some((m) => m.name === People.embModel)) People.embModel = null;
    for (const m of tiers) {
      const option = element("option", "", m.name);
      option.value = m.name;
      option.selected = m.name === (People.embModel || "w600k_mbf.onnx");
      select.append(option);
    }
    if (!People.embModel) People.embModel = select.value;
    select.addEventListener("change", () => { People.embModel = select.value; });
    view.append(select);
    view.append(element("p", "note people-copy",
      "The tier the record names — bigger tiers are more accurate, slower to scan, and a "
      + "larger download. One tier per face store; the record names it."));
  } else {
    view.append(element("p", "note",
      "The pinned face packs are not installed — the button below fetches them from Hugging Face; the People tab stays closed until they are in."));
    view.append(peopleDownloadRow());
  }
  const line = element("div", "people-line");
  const label = element("label", "people-ack");
  const box = element("input");
  box.type = "checkbox";
  box.checked = People.ack;
  box.title = "Face recognition runs on this computer's folders, with the tier named above";
  box.addEventListener("change", () => { People.ack = box.checked; accept.disabled = !box.checked || tiers.length === 0; });
  label.append(box, document.createTextNode(" I understand and accept"));
  const accept = element("button", "primary", "Accept");
  accept.title = "Record the acceptance and open the People tab";
  accept.disabled = !People.ack || tiers.length === 0;
  accept.onclick = peopleAccept;
  line.append(label, accept);
  view.append(line);
  if (People.error) view.append(element("p", "note problem", People.error));
  return view;
}

/// The packs' download row: the engine's own bytes/total, no invented numbers.
function peopleDownloadRow() {
  const row = element("div", "people-download");
  const button = element("button", "", "Download face models…");
  button.onclick = peopleDownload;
  row.append(button);
  const state = element("span", "people-download-state");
  row.append(state);
  paintDownloadLine(state);
  return row;
}

function paintDownloadLine(state) {
  const d = People.download;
  state.textContent = !d ? ""
    : d.state === "downloading" ? d.from ? `Downloading ${d.from}…`
      : `Downloading · ${d.total ? Math.floor((100 * d.bytes) / d.total) : 0}%`
    : d.state === "checking" ? "Checking files…"
    : d.state === "waiting" ? "Waiting for the network"
    : d.state === "failed" ? `The download stopped: ${d.message}`
    : "";
}

function peopleDownloadLine() {
  const state = document.querySelector(".people-download-state");
  if (state) paintDownloadLine(state);
}

function peopleProgressLine() {
  const line = $("people-progress");
  if (line && People.progress) {
    const p = People.progress;
    line.textContent = `checked ${p.checked.toLocaleString()} file${p.checked === 1 ? "" : "s"}`
      + (p.faces ? ` · ${p.faces.toLocaleString()} face${p.faces === 1 ? "" : "s"} found` : "")
      + (p.cluster ? ` · ${p.cluster.assigned.toLocaleString()} assigned, ${p.cluster.persons.toLocaleString()} people` : "")
      + (p.last ? ` · ${nameOf(p.last)}` : "");
  }
}

/// consented: the scan, the counts, the cards, the queue.
function peopleConsentedView() {
  const view = element("div", "people-view");
  const c = People.counts || { faces: 0, persons: 0, pending: 0 };
  view.append(element("h3", "", "Face scan"));
  const state = element("p", "note", "");
  if (People.scanning) {
    state.textContent = "scanning — the engine's clock, seconds per photo…";
  } else if (People.scan && People.scan.error) {
    state.className = "note problem";
    state.textContent = `scan: ${People.scan.error}`;
  } else if (People.scan) {
    const r = People.scan;
    state.textContent = `✓ last scan: ${r.files.toLocaleString()} file${r.files === 1 ? "" : "s"} · `
      + `${r.faces.toLocaleString()} face${r.faces === 1 ? "" : "s"}`
      + (r.fresh ? ` · ${r.fresh.toLocaleString()} fresh` : "")
      + (r.skipped ? ` · ${r.skipped.toLocaleString()} unreadable` : "");
  }
  const progress = element("p", "note", "");
  progress.id = "people-progress";
  const line = element("div", "people-line");
  const scanButton = element("button", "primary", "Scan now");
  scanButton.title = "Scan the whole folder for faces — one long run; files already recorded are skipped, so the next press resumes";
  scanButton.disabled = People.scanning;
  scanButton.onclick = peopleScan;
  const withdraw = element("button", "", "Withdraw consent");
  withdraw.title = "Withdraw the acceptance — the People verbs close";
  withdraw.onclick = () => { People.withdrawing = true; peopleRender(); };
  line.append(scanButton, withdraw);
  const badges = element("span", "people-facts");
  badges.replaceChildren(
    peopleBadge(`${c.faces.toLocaleString()} faces`, c.faces > 0),
    peopleBadge(`${c.persons.toLocaleString()} people`, c.persons > 0),
    peopleBadge(`${c.pending.toLocaleString()} to review`, c.pending > 0),
  );
  if (People.counts && People.counts.models && People.counts.models.emb) {
    badges.append(peopleBadge(People.counts.models.emb, true));
  }
  line.append(badges);
  view.append(state, progress, line);
  if (People.withdrawing) view.append(peopleConfirm(
    "Withdraw face-recognition consent? The People verbs close; the face store stays on disk until you delete it.",
    "Withdraw", peopleWithdraw, () => { People.withdrawing = false; peopleRender(); }));
  if (People.error) view.append(element("p", "note problem", People.error));
  peopleProgressLine();

  // Round six (the user's brief): a clicked person FILLS the view — the gallery scrolls
  // the page and loads on scroll; the grid and the queue come back with "← People".
  const detailRow = People.detail ? People.persons.find((x) => x.id === People.detail) : null;
  if (People.detail && !detailRow) { People.detail = null; People.facesFor = null; }
  if (detailRow && People.facesFor && People.facesFor.person === detailRow.id) {
    view.append(peopleDetailView(detailRow));
    return view;
  }

  view.append(element("h3", "", "People"));
  if (People.persons.length) {
    const grid = element("div", "people-grid");
    // Finding-1 cure (2026-11-23): a cap on CARDS, not a scroll box — the panel
    // paints the people with the most photos first (the family members you are
    // looking for), the PAGE scrolls. The store keeps everyone; this is paint.
    const sorted = People.persons.length > GRID_CAP
      ? [...People.persons].sort((a, b) => b.count - a.count || a.id - b.id)
      : People.persons;
    for (const person of sorted.slice(0, GRID_CAP)) grid.append(peopleCard(person));
    view.append(grid);
    // The cap note counts the STORE (the counts verb), not the window — the window
    // is exactly what we asked for, the store keeps everyone.
    const total = People.counts?.persons ?? People.persons.length;
    if (total > GRID_CAP) {
      view.append(element("p", "note",
        `Showing the top ${GRID_CAP.toLocaleString()} of ${total.toLocaleString()} people — the most-photo people first.`));
    }
  } else {
    view.append(element("p", "note", "Nothing found yet — press Scan now. One press runs the whole folder; files already recorded are skipped."));
  }

  if (People.pending.length) {
    view.append(element("h3", "", "To review"));
    view.append(element("p", "note",
      "Faces with the closest candidate lead the queue — the top rows are the ones you can confirm."));
    const list = element("div", "people-pending");
    for (const item of People.pending) list.append(peoplePendingRow(item));
    view.append(list);
  }
  return view;
}

function peopleConfirm(text, label, go, cancel) {
  const box = element("div", "confirm");
  box.append(element("p", "", text));
  const actions = element("div", "actions");
  const cancelButton = element("button", "", "Cancel");
  cancelButton.onclick = cancel;
  const goButton = element("button", "danger", label);
  goButton.onclick = go;
  actions.append(cancelButton, goButton);
  box.append(actions);
  return box;
}

function peopleCard(person) {
  const card = element("div", "person-card");
  const expanded = People.facesFor && People.facesFor.person === person.id;
  const crop = faceCrop(person.cover_face, 512);
  crop.setAttribute("role", "button");
  crop.setAttribute("tabindex", "0");
  crop.setAttribute("aria-expanded", String(Boolean(expanded)));
  crop.title = `Show the ${person.count} photo${person.count === 1 ? "" : "s"} of ${personLabel(person)}`;
  crop.onclick = () => peopleOpenPerson(person.id);
  crop.onkeydown = (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); peopleOpenPerson(person.id); } };
  card.append(crop);
  const nameBox = peopleNameBox(person);
  card.append(nameBox.name, nameBox.list);
  const facts = element("span", "people-facts");
  facts.append(peopleBadge(`${person.count} photo${person.count === 1 ? "" : "s"}`, person.count > 1));
  card.append(facts);
  // (finding-1 cure 2026-11-23: the merge picker lives in the section strip, not
  // in the card — a card-side select listed EVERY other person, N² options.)
  const del = element("button", "person-delete", "Delete");
  del.title = "Dissolve this person — their faces move to the review queue, no photo is deleted";
  del.onclick = () => { People.deleting = person.id; peopleRender(); };
  card.append(del);
  if (People.deleting === person.id) {
    card.append(peopleConfirm(
      `Delete person #${person.id}? Their faces move to the review queue — no photo is deleted.`,
      "Delete",
      () => { People.deleting = null; peopleVerb("faces_delete", { person: person.id }, "delete"); },
      () => { People.deleting = null; peopleRender(); }));
  }
  return card;
}

function peoplePendingRow(item) {
  const row = element("div", "pending-item");
  row.append(faceCrop(item.face, 512));
  const side = element("span", "pending-side");
  side.append(element("span", "pending-file", nameOf(item.face.path)));
  const cands = element("span", "pending-cands");
  for (const candidate of (item.candidates || []).slice(0, 3)) {
    const button = element("button", "", `${personLabel({ id: candidate.person, name: candidate.name })} · ${candidate.cos.toFixed(2)}`);
    button.title = `The engine's cosine to this person: ${candidate.cos}`;
    button.onclick = () => peopleVerb("faces_confirm", { face: item.face.id, person: candidate.person }, "confirm");
    cands.append(button);
  }
  if (!(item.candidates || []).length) cands.append(element("span", "note", "no close person — it stays in the queue"));
  side.append(cands);
  row.append(side);
  return row;
}
