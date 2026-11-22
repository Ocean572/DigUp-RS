// digup-people — the People tab panel (FACES.md Phase 3): the consent gate, the scan,
// the person cards, the review queue. It rides the gear panel's measured frame (same
// anchor under the header's right edge, the 2 rem rows, overflow-x: clip — the section
// rhythm values are the settings panel's brief, restated here because component CSS owns
// the shadow interior). Thin-client law: the panel renders the store slice the
// people-controller wrote from the engine's replies and invents nothing — the crop is
// pure geometry (box/width/height relayed, mapped onto the existing /preview handle).
//
// The consent gate mirrors the server's wall (FACES.md §5 "what the UI gates, the engine
// gates"): an unaccepted deployment sees ONLY the enable screen — no counts, no cards,
// no scan button; the box is unchecked by default and Accept sends the affirmative act.
// The crop math is scale-invariant and lives in lib/faces.js: /preview scales the photo
// by pixels/max(W,H), the box rides source pixels, so the percentages box→photo cancel
// the scale — the image is enlarged to 100%*W/cw inside a cw/ch-aspect window, offset
// by −cx0/cw, −cy0/ch (cropGeometry is exported pure, its numbers pinned by unit tests).
import { LitElement, html, css, nothing } from "lit";
import { storeController } from "../app/store.js";
import { controlCss, badgeCss, spinCss, noteCss } from "../styles/controls.js";
import { baseName } from "../lib/format.js";
import { faceCrop, DETECTOR } from "../lib/faces.js";

const icon = (paths) => html`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor"
  stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
  focusable="false">${paths}</svg>`;
const USERS_ICON = icon(html`
  <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"></path><circle cx="9" cy="7" r="4"></circle>
  <path d="M22 21v-2a4 4 0 0 0-3-3.87"></path><path d="M16 3.13a4 4 0 0 1 0 7.75"></path>`);
const SCAN_ICON = icon(html`
  <path d="M3 7V5a2 2 0 0 1 2-2h2M17 3h2a2 2 0 0 1 2 2v2M21 17v2a2 2 0 0 1-2 2h-2M7 21H5a2 2 0 0 1-2-2v-2"></path>
  <circle cx="12" cy="12" r="3.5"></circle>`);
const QUEUE_ICON = icon(html`
  <path d="M9 6h11M9 12h11M9 18h11"></path><circle cx="4.5" cy="6" r="1"></circle>
  <circle cx="4.5" cy="12" r="1"></circle><circle cx="4.5" cy="18" r="1"></circle>`);

const personLabel = (p) => p.name || `Person ${p.id}`;

class DigupPeople extends LitElement {
  static properties = { ack: { state: true }, openId: { state: true } };

  static styles = [controlCss, badgeCss, spinCss, noteCss, css`
    /* The gear panel's measured frame, restated for this panel (component CSS owns the
       shadow interior): same anchor, same clip rule, wider for the card grid. */
    :host([hidden]) { display: none; }
    :host { position: absolute; right: 1.25rem; top: calc(100% + 0.45rem); z-index: var(--z-panel);
      width: min(34rem, calc(100vw - 2rem));
      display: flex; flex-direction: column; gap: 0; padding: 0.9rem 0.95rem 1rem;
      background: var(--panel); border: 1px solid var(--edge); border-radius: var(--radius-lg);
      box-shadow: 0 8px 28px rgb(0 0 0 / 22%);
      /* Round-7 law (pinned by test_media_ui on the served bundle): the panel NEVER
         scrolls — a scroll box is a clipping box (controls cut at the edges, the
         scrollbar steals width). No max-height, no overflow-y: a long queue grows
         the panel and the PAGE scrolls, the gear panel's own discipline. */
      overflow-x: clip; }
    .set-head { display: flex; justify-content: space-between; align-items: center;
      font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.05em; color: var(--soft); }
    #people-close { padding: 0.1rem 0.55rem; font-size: 0.8rem; line-height: 1.2; }
    .set-sec { display: grid; gap: var(--space-sm); padding-top: var(--space-md);
      margin-top: var(--space-md); border-top: 1px solid var(--line); }
    .set-sec:first-of-type { border-top: 0; padding-top: var(--space-sm); margin-top: var(--space-sm); }
    .set-title { display: flex; align-items: center; gap: 0.45rem; font-size: 0.95rem;
      font-weight: 600; letter-spacing: 0.01em; color: var(--text); }
    .set-title svg { width: 15px; height: 15px; flex: none; color: var(--accent); }
    .set-line { display: flex; align-items: center; gap: var(--space-md); flex-wrap: wrap;
      min-height: var(--row-height); box-sizing: border-box; }
    .set-line label { display: inline-flex; align-items: center; gap: 0.4rem;
      min-height: var(--row-height); color: var(--soft); font-size: var(--text-sm); }
    #people-state { display: inline-flex; align-items: center; gap: 0.45rem; min-width: 0;
      font-size: var(--text-sm); overflow-wrap: anywhere; }
    .state-done { color: var(--good); }
    .state-err { color: var(--warn); }
    .badge.ok { color: var(--good); border-color: color-mix(in srgb, var(--good) 55%, var(--edge)); }
    .badge.off { color: var(--soft); }
    .scan-clock { color: var(--soft); font-size: var(--text-sm); }

    /* The consent gate: the enable screen FACES.md §5 pins (no pre-ticked box; Accept
       rides the box). */
    #consent-copy { color: var(--soft); font-size: var(--text-sm); margin: 0; max-width: 30rem; }
    #consent-accept:disabled { opacity: 0.55; cursor: default; }
    select#consent-emb { width: 100%; max-width: 100%; }

    /* Person cards: the auto-fill grid, the crop window, the 2 rem control rows. */
    #people-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(9rem, 1fr));
      gap: var(--space-sm); }
    .person-card { display: grid; gap: 0.35rem; padding: 0.5rem; min-width: 0;
      border: 1px solid var(--edge); border-radius: var(--radius-lg); background: var(--bg); }
    .crop { position: relative; display: block; overflow: hidden; width: 100%;
      border: 1px solid var(--edge); border-radius: var(--radius-md); background: var(--field);
      cursor: pointer; }
    .crop img { position: absolute; max-width: none; }
    img.plain { display: block; width: 100%; border: 1px solid var(--edge);
      border-radius: var(--radius-md); }
    .person-name { font: inherit; min-width: 0; min-height: var(--row-height); box-sizing: border-box;
      background: var(--field); color: var(--text); border: 1px solid var(--edge);
      border-radius: var(--radius-md); padding: 0.42rem 0.6rem; }
    .person-card select, .person-card input, .person-card button { width: 100%; box-sizing: border-box; }
    .face-strip { display: grid; grid-template-columns: repeat(auto-fill, minmax(4rem, 1fr));
      gap: 0.3rem; }

    /* The review queue: a face + the engine's candidate cosines, one row per item. */
    .pending-item { display: grid; grid-template-columns: 4.5rem minmax(0, 1fr);
      gap: 0.5rem; align-items: start; padding: 0.5rem; min-width: 0;
      border: 1px solid var(--edge); border-radius: var(--radius-lg); background: var(--bg); }
    .pending-file { display: block; color: var(--soft); font-size: var(--text-xs);
      overflow-wrap: anywhere; }
    .pending-cands { display: flex; gap: 0.35rem; flex-wrap: wrap; margin-top: 0.3rem; }

    @media (max-width: 720px) { :host { width: calc(100vw - 2rem); } }
  `];

  constructor() {
    super();
    this.ack = false;
    this.openId = null;
    this._sel = null;
  }

  set store(store) {
    if (store === this._store) return;
    this._store = store;
    this._sel = storeController(this, store, (s) => s.people);
  }
  get store() { return this._store; }

  _p() { return this._sel ? this._sel.value() : null; }

  #emit(type, detail) {
    this.dispatchEvent(new CustomEvent(type, { bubbles: true, composed: true, detail }));
  }

  #toggle(person) {
    if (this.openId === person) {
      this.openId = null;
      this.#emit("digup:close-person", { person });
    } else {
      this.openId = person;
      this.#emit("digup:open-person", { person });
    }
  }

  render() {
    const p = this._p();
    if (!p) return null;
    return html`
      <div class="set-head">
        <span>People</span>
        <button id="people-close" type="button" aria-label="Close people panel"
                @click=${this.#close}>✕</button>
      </div>
      ${p.consented ? this.#consented(p) : this.#consentGate(p)}`;
  }

  /* ── the enable screen (FACES.md §5, the default-OFF posture) ── */
  #consentGate(p) {
    const tiers = p.models.filter((m) => m.name !== DETECTOR);
    const present = tiers.filter((m) => m.present);
    return html`
      <section class="set-sec" aria-labelledby="people-gate">
        <span class="set-title" id="people-gate">${USERS_ICON} Face recognition is off</span>
        <p id="consent-copy">Face recognition finds faces, groups them into people, and crops
          the photos. It runs only after you accept, only on this deployment's own folders,
          only with the user-supplied packs below. Your acceptance is kept as a file
          (0600) and you can withdraw it at any time; withdrawing never deletes photos.</p>
        <span class="set-facts">
          ${p.models.map((m) => html`<span class="badge ${m.present ? "ok" : "off"}"
                    title=${m.present ? "installed in the models folder" : "not installed"}>${m.name}</span>`)}
        </span>
        ${present.length ? html`
          <select id="consent-emb" title="The embedder tier the record names — one per face.sqlite"
                  @change=${(ev) => { this.embModel = ev.target.value; }}>
            ${present.map((m) => html`<option value=${m.name} ?selected=${m.name === "w600k_mbf.onnx"}>${m.name}</option>`)}
          </select>`
        : html`<p class="note warn-text">The pinned face packs are not installed —
            <code>digup download-model --faces</code> fetches them; the People tab stays
            closed until then.</p>`}
        <div class="set-line">
          <label title="Face recognition runs on this deployment's folders, with the tier named above">
            <input id="consent-ack" type="checkbox" .checked=${this.ack}
                   @change=${(ev) => { this.ack = ev.target.checked; }}>
            I understand and accept
          </label>
          <button id="consent-accept" type="button"
                  title="Record the acceptance and open the People tab"
                  ?disabled=${!this.ack || present.length === 0}
                  @click=${() => this.#emit("digup:consent",
                    { embModel: this.embModel || (present.some((m) => m.name === "w600k_mbf.onnx")
                      ? "w600k_mbf.onnx" : present[0].name) })}>Accept</button>
        </div>
      </section>`;
  }

  /* ── consented: the scan, the counts, the cards, the queue ── */
  #consented(p) {
    const c = p.counts || { faces: 0, persons: 0, pending: 0, named: 0 };
    const scan = p.scan || { spinning: false, result: null, error: null };
    let state = null;
    if (scan.spinning) state = { spin: true, text: "scanning — the engine's clock, seconds per photo…" };
    else if (scan.error) state = { err: true, text: `scan: ${scan.error}` };
    else if (scan.result) {
      const r = scan.result;
      state = { done: true, text: `${r.files} file${r.files === 1 ? "" : "s"} · ${r.faces} face${r.faces === 1 ? "" : "s"}`
                 + (r.fresh ? ` · ${r.fresh} fresh` : "")
                 + (r.skipped ? ` · ${r.skipped} unreadable` : "") };
    }
    return html`
      <section class="set-sec" aria-labelledby="people-scan">
        <div class="set-head-row" style="display:flex;justify-content:space-between;
                                          align-items:center;gap:0.5rem;flex-wrap:wrap">
          <span class="set-title" id="people-scan">${SCAN_ICON} Face scan</span>
          ${state ? html`<span id="people-state">
            ${state.spin ? html`<span class="estimate-spin" aria-label="working"></span>` : null}
            ${state.done ? html`<span class="state-done" aria-hidden="true">✓</span>` : null}
            ${state.err ? html`<span class="state-err" aria-hidden="true">!</span>` : null}
            <span>${state.text}</span></span>` : null}
        </div>
        <div class="set-line">
          <button id="faces-scan" type="button" ?disabled=${scan.spinning}
                  title="Scan the folders for faces — files already recorded are skipped"
                  @click=${() => this.#emit("digup:scan", {})}>Scan now</button>
          <span class="scan-clock">
            <span class="badge ${c.faces ? "ok" : "off"}">${c.faces} faces</span>
            <span class="badge ${c.persons ? "ok" : "off"}">${c.persons} people</span>
            <span class="badge ${c.pending ? "ok" : "off"}">${c.pending} to review</span>
            ${c.models && c.models.emb ? html`<span class="badge">${c.models.emb}</span>` : nothing}
          </span>
          <button id="faces-withdraw" type="button" title="Withdraw the acceptance — the People verbs close"
                  @click=${() => this.#emit("digup:withdraw", {})}>Withdraw consent</button>
        </div>
        ${p.error ? html`<p class="note warn-text">${p.error}</p>` : nothing}
      </section>

      <section class="set-sec" aria-labelledby="people-people">
        <span class="set-title" id="people-people">${USERS_ICON} People</span>
        ${p.persons.length ? html`
          <div id="people-grid">
            ${p.persons.map((person) => this.#card(person, p))}
          </div>`
        : html`<p class="note">Nothing found yet — press Scan now. A scan runs on the
            engine's clock; files already recorded are skipped.</p>`}
      </section>

      <section class="set-sec" id="pending-sec" aria-labelledby="people-pending"
               ?hidden=${p.pending.length === 0}>
        <span class="set-title" id="people-pending">${QUEUE_ICON} To review</span>
        <div id="pending-list">
          ${p.pending.map((item) => html`
            <div class="pending-item">
              ${faceCrop(item.face)}
              <span>
                <span class="pending-file">${baseName(item.face.path)}</span>
                <span class="pending-cands">
                  ${item.candidates.slice(0, 3).map((c) => html`
                    <button type="button" title=${`The engine's cosine to this person: ${c.cos}`}
                            @click=${() => this.#emit("digup:confirm",
                              { face: item.face.id, person: c.person })}>
                      ${personLabel({ id: c.person, name: c.name })} · ${c.cos.toFixed(2)}</button>`)}
                  ${item.candidates.length === 0
                    ? html`<span class="note">no close person — it stays in the queue</span>` : nothing}
                </span>
              </span>
            </div>`)}
        </div>
      </section>`;
  }

  #card(person, p) {
    const others = p.persons.filter((x) => x.id !== person.id);
    const expanded = p.facesFor && p.facesFor.person === person.id;
    return html`
      <div class="person-card" id=${`person-${person.id}`}>
        <span class="crop" role="button" tabindex="0"
              aria-expanded=${String(Boolean(expanded))}
              title=${`Show the ${person.count} photo${person.count === 1 ? "" : "s"} of ${personLabel(person)}`}
              @click=${() => this.#toggle(person.id)}
              @keydown=${(ev) => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); this.#toggle(person.id); } }}>
          ${faceCrop(person.cover_face)}
        </span>
        <input class="person-name" type="text" .value=${person.name || ""}
               placeholder=${`Person ${person.id}`} aria-label=${`Name for person ${person.id}`}
               @change=${(ev) => this.#emit("digup:name",
                 { person: person.id, name: ev.target.value })}>
        <span class="set-facts">
          <span class="badge ${person.count > 1 ? "ok" : "off"}">${person.count} photo${person.count === 1 ? "" : "s"}</span>
        </span>
        <select class="person-merge" aria-label=${`Merge ${personLabel(person)} into…`}
                ?disabled=${others.length === 0}
                @change=${(ev) => {
                  const into = Number(ev.target.value);
                  ev.target.value = "";                       // the picker is one-shot
                  if (into) this.#emit("digup:merge", { into, from: person.id });
                }}>
          <option value="">Merge into…</option>
          ${others.map((o) => html`<option value=${String(o.id)}>${personLabel(o)}</option>`)}
        </select>
        <button class="person-delete" type="button"
                title="Dissolve this person — their faces move to the review queue, no photo is deleted"
                @click=${() => this.#emit("digup:delete", { person: person.id })}>Delete</button>
        ${expanded ? html`
          <div class="face-strip">
            ${p.facesFor.faces.map((f) => faceCrop(f))}
          </div>` : nothing}
      </div>`;
  }

  #close = () => this.#emit("digup:close", {});

  focusFirst() {
    const first = this.renderRoot.querySelector("select, button, input");
    first?.focus();
  }
}
if (!customElements.get("digup-people")) customElements.define("digup-people", DigupPeople);
