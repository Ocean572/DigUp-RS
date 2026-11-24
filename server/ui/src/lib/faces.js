// faces.js — the People tab's crop templates (the media.js pattern: templates + pure
// helpers, no custom element). The thin-client crop law (FACES.md Phase 3): the engine
// relays `box`/`kps` in SOURCE pixels plus the photo's own `width`/`height` (face.sqlite
// schema 3), and the ONLY media handle is the scaled `/preview`. The mapping is
// scale-invariant: /preview scales the photo by pixels/max(W,H), the box rides source
// px, so the box→photo ratios cancel the scale exactly — the window is cw×ch source
// px shown by enlarging the image W/cw and offsetting −cx0/cw, −cy0/ch. No crop verbs,
// no invented numbers: the geometry is pure arithmetic over relayed rows.
import { html } from "lit";

export const DETECTOR = "scrfd_10g_bnkps.onnx";

/* The crop's window geometry, exported pure (the unit tests pin these numbers):
   head-and-shoulders padding over the engine's box, clamped to the photo's own pixels.
   A row with no map (width 0 — recorded before schema 3) has no window: null, the
   whole-photo fallback FACES.md pins (a re-scan of a changed file fills the map). */
export function cropGeometry(row) {
  const [x0, y0, x1, y1] = row.box;
  const W = row.width, H = row.height;
  if (!(W > 0 && H > 0) || !(x1 > x0) || !(y1 > y0)) return null;
  const bw = x1 - x0, bh = y1 - y0;
  const cx0 = Math.max(0, x0 - bw * 0.45), cx1 = Math.min(W, x1 + bw * 0.45);
  const cy0 = Math.max(0, y0 - bh * 0.6), cy1 = Math.min(H, y1 + bh * 0.8);
  const cw = Math.max(cx1 - cx0, 1), ch = Math.max(cy1 - cy0, 1);
  return { cw, ch, cx0, cy0, W };
}

/* The face crop: a cw/ch window over the enlarged photo (see header). */
export const faceCrop = (row, pixels = 512) => {
  const src = `/api/v1/media/${row.media_id}/preview?pixels=${pixels}`;
  const g = cropGeometry(row);
  if (!g) return html`<img class="plain" src=${src} alt=${""} loading="lazy">`;
  const { cw, ch, cx0, cy0, W } = g;
  return html`<span class="crop" style=${`aspect-ratio: ${cw / ch}`}>
    <img src=${src} alt=${""} loading="lazy"
         style=${`width: calc(100% * ${W / cw}); left: calc(-100% * ${cx0 / cw}); top: calc(-100% * ${cy0 / ch})`}>
  </span>`;
};
