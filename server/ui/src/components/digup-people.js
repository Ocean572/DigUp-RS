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
// The panel paints at most this many person cards (finding-1 cure 2026-11-23,
// measured on the bench library: 37k cards are unpaintable — the cap is on
// PAINT, every person stays in the store; most-photo people first).
const GRID_CAP = 300;

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
    /* The merge strip (finding-1 cure 2026-11-23): one row of two native selects +
       the Merge act — native dropdowns, no scroller, no height cap (the panel's
       round-7 law). */
    /* Round six (2026-11-23): the merge strip is GONE (typing the same name is the merge
       act — the datalist). The person's own view: photo-sized crops, the page scrolls. */
    .detail-head { display: flex; flex-wrap: wrap; gap: 0.5rem; align-items: center; }
    .detail-head .person-name { flex: 1 1 8rem; min-width: 0; }
    .detail-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(8rem, 1fr));
      gap: 0.5rem; }
    .detail-sentinel { height: 1px; }
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

  /* Infinite scroll (round six, the user's law: keep scrolling, load as you go, nothing
     loads until you scroll): the gallery's foot sentinel, watched by the PAGE viewport —
     the panel never scrolls (round-seven law stands), the page does. jsdom has no
     IntersectionObserver (tests click "Show 50 more"); browsers have it. */
  updated() {
    const sentinel = this.shadowRoot.querySelector(".detail-sentinel");
    if (this._io) { this._io.disconnect(); this._io = null; }
    if (!sentinel || typeof IntersectionObserver === "undefined") return;
    this._io = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) this.#emit("digup:more-person", { person: this.openId });
      }
    }, { rootMargin: "600px 0px" });
    this._io.observe(sentinel);
  }
  disconnectedCallback() {
    super.disconnectedCallback();
    if (this._io) { this._io.disconnect(); this._io = null; }
  }

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
        <span class="set-title" id="people-gate">${USERS_ICON} Face recognition is off — what switching it on does</span>
        <p id="consent-copy">Face recognition runs on two model packs: a detector that finds
          faces, and an embedder that turns each face into numbers. They are plain files the
          server downloads once from Hugging Face into its models folder. That download is the
          only network step: face recognition then runs entirely on this machine. Photos are
          never uploaded; what a face becomes — numbers and crops — is stored only in the
          server's own face store beside its index. No account, no cloud.</p>
        <p id="consent-copy">Accepting records your permission, as a file (0600) in the
          server's config: it may find faces in the photos of this deployment's folders,
          describe each face with the tier chosen below, and group matching faces into people
          you can name. You can withdraw at any time — scanning stops, and withdrawing never
          deletes photos.</p>
        <span class="set-facts">
          ${p.models.map((m) => html`<span class="badge ${m.present ? "ok" : "off"}"
                    title=${m.present ? "installed in the models folder" : "not installed"}>${m.name}</span>`)}
        </span>
        ${present.length ? html`
          <select id="consent-emb" title="The embedder tier the record names — one per face.sqlite"
                  @change=${(ev) => { this.embModel = ev.target.value; }}>
            ${present.map((m) => html`<option value=${m.name} ?selected=${m.name === (this.embModel || "w600k_mbf.onnx")}>${m.name}</option>`)}
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
        ${p.facesFor && p.facesFor.person === this.openId ? this.#detail(p) : html`
          <span class="set-title" id="people-people">${USERS_ICON} People</span>
          ${p.persons.length ? html`
            <div id="people-grid">
              ${(p.persons.length > GRID_CAP
                ? [...p.persons].sort((a, b) => b.count - a.count || a.id - b.id)
                : p.persons
              ).slice(0, GRID_CAP).map((person) => this.#card(person, p))}
            </div>
            ${(p.counts?.persons ?? p.persons.length) > GRID_CAP ? html`
              <p class="note">Showing the top ${GRID_CAP.toLocaleString()} of
              ${(p.counts?.persons ?? p.persons.length).toLocaleString()} people — the most-photo people first.</p>` : nothing}`
          : html`<p class="note">Nothing found yet — press Scan now. A scan runs on the
              engine's clock; files already recorded are skipped.</p>`}
        `}
      </section>

      <section class="set-sec" id="pending-sec" aria-labelledby="people-pending"
               ?hidden=${p.pending.length === 0 || Boolean(p.facesFor && p.facesFor.person === this.openId)}>
        <span class="set-title" id="people-pending">${QUEUE_ICON} To review</span>
        <p class="note">Faces with the closest candidate lead the queue — the top rows are the ones you can confirm.</p>
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

  /* The person's OWN view (round six, the user's brief 2026-11-23): the gallery fills the
     panel — newest photo first, 50 at a time; the PAGE scrolls (the panel-never-scrolls law
     stands — no scroller, no height cap), the next page loads when the foot sentinel
     reaches the viewport. The grid's rows are never squeezed by an inline strip; the merge
     picker is GONE (typing the same name is the merge act — the datalist, not a strip). */
  #detail(p) {
    const person = p.persons.find((x) => x.id === this.openId);
    if (!person) return nothing;
    const shown = p.facesFor.faces.length;
    return html`
      <div class="detail-head">
        <button type="button" title="Back to the people grid"
                @click=${() => this.#toggle(person.id)}>← People</button>
        <span class="set-title">${personLabel(person)}</span>
        <span class="badge ${person.count > 1 ? "ok" : "off"}">${person.count.toLocaleString()} photo${person.count === 1 ? "" : "s"}</span>
        ${this.#nameInput(person, p)}
      </div>
      <div class="detail-grid">
        ${p.facesFor.faces.map((f) => faceCrop(f))}
      </div>
      ${p.facesFor.loading ? html`<p class="note">Loading the next photos…</p>`
        : p.facesFor.more ? html`
          <p class="note">Keep scrolling — the next 50 photos load as you go
          (${shown.toLocaleString()} of ${person.count.toLocaleString()} shown).</p>
          <button class="face-more" type="button" title="The next 50 photos, newest first"
                  ?disabled=${p.facesFor.loading}
                  @click=${() => this.#emit("digup:more-person", { person: this.openId })}>
            ${p.facesFor.loading ? "Loading…" : "Show 50 more"}</button>
          <div class="detail-sentinel"></div>`
        : html`<p class="note">All ${shown.toLocaleString()} photos shown — newest first.</p>`}`;
  }

  #nameInput(person, p) {
    return html`
      <input class="person-name" type="text" .value=${person.name || ""}
             list=${`names-${person.id}`}
             placeholder=${`Person ${person.id}`} aria-label=${`Name for person ${person.id}`}
             @change=${(ev) => {
               const text = ev.target.value.trim();
               // Autocomplete law (2026-11-23): picking a name the store already has
               // (exact match from the named list) is the human's merge act — merge
               // this person INTO the one that carries the name. A fresh name is the
               // plain name act. The panel relays; the engine merges.
               const target = (p.names || []).find((n) => n.name === text && n.id !== person.id);
               if (target) this.#emit("digup:merge", { into: target.id, from: person.id });
               else this.#emit("digup:name", { person: person.id, name: ev.target.value });
             }}>
      <datalist id=${`names-${person.id}`}>
        ${(p.names || []).filter((n) => n.id !== person.id)
          .map((n) => html`<option value=${n.name}>${`${n.name} (${n.count})`}</option>`)}
      </datalist>`;
  }

  #card(person, p) {
    return html`
      <div class="person-card" id=${`person-${person.id}`}>
        <span class="crop" role="button" tabindex="0"
              title=${`Show the ${person.count} photo${person.count === 1 ? "" : "s"} of ${personLabel(person)}`}
              @click=${() => this.#toggle(person.id)}
              @keydown=${(ev) => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); this.#toggle(person.id); } }}>
          ${faceCrop(person.cover_face)}
        </span>
        ${this.#nameInput(person, p)}
        <span class="set-facts">
          <span class="badge ${person.count > 1 ? "ok" : "off"}">${person.count} photo${person.count === 1 ? "" : "s"}</span>
        </span>
        <!-- Round six (2026-11-23): NO inline strip and NO merge strip — the gallery is
             the person's own view (no row-squeeze) and typing the same name IS the merge. -->
        <button class="person-delete" type="button"
                title="Dissolve this person — their faces move to the review queue, no photo is deleted"
                @click=${() => this.#emit("digup:delete", { person: person.id })}>Delete</button>
      </div>`;
  }

  #close = () => this.#emit("digup:close", {});

  focusFirst() {
    const first = this.renderRoot.querySelector("select, button, input");
    first?.focus();
  }
}
if (!customElements.get("digup-people")) customElements.define("digup-people", DigupPeople);
