// digup-preview — the facts block's `Find like this` (#162/#121, 2026-11-25): the
// photo rides the REST law as its minted media handle (paths never cross the API);
// the shell listens for digup:find-like. Pictures only — the engine has no picture
// vector for a document/audio/video row, so facts blocks for those carry no dead
// button (lib/media.js factsTemplate, shared with the embed — preview and embed
// keep it byte-identical).
import { describe, expect, it, afterEach } from "vitest";
import "./digup-preview.js";
import { onceEvent } from "../lib/test-util.js";

const photo = { media_id: "p1", kind: "image", path: "/x/a.jpg", z: 3.2 };
const doc = { media_id: "d1", kind: "pdf", path: "/x/report.pdf", segment: 2 };
const clip = { media_id: "v1", kind: "video", path: "/x/c.mp4", loc: 12 };

async function mount(hit) {
  const el = document.createElement("digup-preview");
  el.hit = hit;
  document.body.append(el);
  await el.updateComplete;
  return el;
}

const findButton = (el) => [...el.shadowRoot.querySelectorAll(".facts button")]
  .find((b) => b.textContent.trim() === "Find like this");

describe("digup-preview — the facts Find-like button", () => {
  let el;
  afterEach(() => el?.remove());

  it("a photo's facts block carries the button; the click names the minted handle", async () => {
    el = await mount(photo);
    const button = findButton(el);
    expect(button).not.toBeNull();
    expect(button.title).toBe("Ask the engine for pictures like this one");
    const p = onceEvent(document, "digup:find-like");
    button.click();
    expect((await p).detail.mediaId).toBe("p1");
  });

  it("document and video facts carry no find button (no picture vector, no dead control)", async () => {
    el = await mount(doc);
    expect(findButton(el)).toBeUndefined();
    el.remove();
    el = await mount(clip);
    expect(findButton(el)).toBeUndefined();
  });

  it("Open and Download ride the minted media route (the path stays out of the API)", async () => {
    el = await mount(photo);
    const links = [...el.shadowRoot.querySelectorAll(".facts a")].map((a) => a.getAttribute("href"));
    expect(links.every((href) => href.includes("/api/v1/media/p1/file"))).toBe(true);
  });
});
