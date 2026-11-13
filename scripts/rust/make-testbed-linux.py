#!/usr/bin/env python3
"""The Linux twin of scripts/make-testbed.sh (Stage 5's eval ground truth).

    python3 scripts/rust/make-testbed-linux.py <dir> [--img-cache DIR]

The macOS builder leans on AppKit, `say`, Vision, the system pictures and xattr;
this one builds the SAME folders with the SAME known content through tools every
Linux box has (ffmpeg, espeak-ng, PIL, fpdf2, curl-able Wikimedia):

  synthetic/img/           Wikimedia Commons fixtures, 768 px JPEG (the macOS
                           builder uses Apple's system pictures, which cannot be
                           redistributed — these are the public equivalents)
  synthetic/screenshots/   the six fake UI "screenshots" (make_screenshots.swift,
                           text verbatim; on Linux the engine has no OCR, so these
                           are vision-only there — identical on both engines)
  synthetic/audio/         `say -v Samantha` becomes espeak-ng (en); the sentences
                           are make_media.sh's, verbatim
  synthetic/audio_trunc/   the length-cap clips, same ffmpeg filters
  synthetic/video/         the penguin|zebra|parrot 18 s film + 6 s moments
  synthetic/docs/          make_docs.swift's topics (one per PDF page) and notes
  synthetic/long/          make_long.swift byte-for-byte: the LCG (seed 20251009,
                           multiplier 6364136223846793005, increment
                           1442695040888963407, pick = (seed >> 33) % count) so the
                           standouts land on pages 41/66/88/100, the Lisbon week
                           past 120,000 characters, and 4815162342 at its chunk
  synthetic/lang/{bn,ar}/  make_lang.swift's Bengali and Arabic notes, PDFs
                           (clean text layers: fpdf2 unshaped), screenshots, speech
  traps/                   the crawler cases: real MPEG-TS, duplicates, odd names
                           (U+202F included), MyProject, node_modules, .hidden,
                           an app bundle, tiny icons, a 600-clip dataset
  code/                    scripts/testbed/make_code.py, as the macOS builder calls it

Images are public Wikimedia Commons files fetched through the Commons API (the
Commons picture subjects are the eval ground truth: a penguin photo answers
"a penguin"). Everything generated here is synthetic — no personal data.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import zipfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from getimg import subject_url, get  # the Commons fetcher, same folder

import arabic_reshaper
from bidi.algorithm import get_display
from fpdf import FPDF
from PIL import Image, ImageDraw, ImageFont

NOTO = "/usr/share/fonts/truetype/noto"
FONTS = {
    "sans": f"{NOTO}/NotoSans-Regular.ttf",
    "sans-b": f"{NOTO}/NotoSans-Bold.ttf",
    "mono": f"{NOTO}/NotoSansMono-Regular.ttf",
    "bn": f"{NOTO}/NotoSansBengali-Regular.ttf",
    "bn-b": f"{NOTO}/NotoSansBengali-Bold.ttf",
    "ar": f"{NOTO}/NotoNaskhArabic-Regular.ttf",
    "ar-b": f"{NOTO}/NotoNaskhArabic-Bold.ttf",
}

# The pictures: the eval filename → the Commons subject it searches for.
PICTURES = {
    "Animals-Penguin.jpg": "African penguin",
    "Animals-Zebra.jpg": "plains zebra",
    "Animals-Parrot.jpg": "red-and-green macaw",
    "Animals-Eagle.jpg": "golden eagle in flight",
    "Animals-Owl.jpg": "little owl Athene",
    "Flowers-Sunflower.jpg": "sunflower Helianthus",
    "Flowers-Yellow Daisy.jpg": "yellow daisy flower",
    "Flowers-Dandelion.jpg": "dandelion Taraxacum",
    "Fun-Gingerbread Man.jpg": "gingerbread man",
    "Instruments-Guitar.jpg": "classical guitar",
    "Instruments-Violin.jpg": "violin",
    "Nature-Cactus.jpg": "Echinocactus grusonii",
    "Nature-Earth.jpg": "Earth seen from Apollo 17",
    "Nature-Lightning.jpg": "lightning strike thunderstorm",
    "Nature-Leaf.jpg": "green leaf macro photography",
    "Nature-Zen.jpg": "zen garden raked sand",
    "Sports-Target.jpg": "dartboard",
    "Fun-Medal.jpg": "sports medal",
}

# make_media.sh's sentences, verbatim.
SPEECH = {
    "speech_groceries.wav": "Don't forget to buy milk, eggs and bread on the way home tomorrow.",
    "speech_revenue.wav": "Our quarterly revenue grew by twenty percent, driven by strong sales in Europe.",
    "speech_flight_delay.wav": "Attention passengers. The flight to Tokyo has been delayed by two hours because of a storm.",
    "speech_cake_recipe.wav": (
        "In this recipe we will bake a chocolate cake. First, preheat the oven to one hundred eighty degrees, "
        "then mix the flour, sugar and cocoa powder in a large bowl. Add the eggs, the melted butter and the milk, "
        "and stir until the batter is smooth. Pour it into a greased pan and bake for thirty five minutes. "
        "While the cake is baking, prepare the frosting by whipping cream with a little sugar and vanilla. "
        "When the cake has cooled down completely, spread the frosting on top and decorate it with fresh "
        "strawberries and some grated chocolate."
    ),
}
BN_SPEECH = "আগামীকাল ঢাকায় ভারী বৃষ্টি হবে, তাই বাইরে যাওয়ার সময় ছাতা নিয়ে বের হবেন। বিকেলে বজ্রসহ ঝড়ের সম্ভাবনা আছে।"
AR_SPEECH = "غدا سيكون الطقس ممطرا في الرياض مع رياح قوية، فلا تنس المظلة عند خروجك من البيت."


def sh(args):
    # stdin is /dev/null: ffmpeg's interactive handler (it reads the terminal when
    # it is attached) reads keystrokes there. stderr is captured so a failure names
    # the command and says why (check=True then raises the CalledProcessError).
    child = subprocess.run(args, check=True, stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if child.returncode != 0:
        sys.stderr.write(child.stderr.decode("utf-8", "replace")[-2000:])
    child.check_returncode()


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as file:
        file.write(text)


def copy(src, dst):
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    shutil.copyfile(src, dst)


# MARK: pictures

def build_images(img_cache, out_dir):
    os.makedirs(img_cache, exist_ok=True)
    for name, search in PICTURES.items():
        cached = os.path.join(img_cache, name)
        if not os.path.exists(cached):
            get(subject_url(search), cached)
            im = Image.open(cached).convert("RGB")
            im.thumbnail((768, 768))
            im.save(cached, "JPEG", quality=88)
        copy(cached, os.path.join(out_dir, name))


# MARK: screenshots (make_screenshots.swift, 1440×900, text verbatim)

W, H = 1440, 900
DESKTOP = (92, 115, 158)
RED, YELLOW, GREEN, BLUE, PURPLE, ORANGE, PINK = (255, 59, 48), (255, 204, 0), (52, 199, 89), (0, 122, 255), (175, 82, 222), (255, 149, 0), (255, 55, 148)


def font(kind, size):
    return ImageFont.truetype(FONTS[kind], size)


def shot(name, title, body, title_font="sans", title_color=(110, 110, 115), bg=(255, 255, 255), chrome=(237, 237, 240)):
    canvas = Image.new("RGB", (W, H), DESKTOP)
    d = ImageDraw.Draw(canvas)
    win = (80, 60, W - 80, H - 60)
    d.rectangle(win, fill=bg)
    d.rectangle((win[0], win[1], win[2], win[1] + 44), fill=chrome)
    for i, color in enumerate((RED, YELLOW, GREEN)):
        x = win[0] + 18 + i * 22
        d.ellipse((x, win[1] + 15, x + 13, win[1] + 28), fill=color)
    d.text((win[0] + 200, win[1] + 12), title, font=font(title_font, 15), fill=title_color)
    body(d, (win[0] + 40, win[1] + 70, win[2] - 40, win[3] - 40))
    os.makedirs(os.path.dirname(name), exist_ok=True)
    canvas.save(name)


def shot_payment_error(path):
    def body(d, r):
        d.text((r[0], r[1]), "Invoices", font=font("sans-b", 30), fill=(0, 0, 0))
        d.rectangle((r[0], r[1] + 46, r[2], r[1] + 126), fill=(255, 230, 230))
        d.text((r[0] + 24, r[1] + 62), "⚠  Payment failed: Your card was declined (card_declined)", font=font("sans-b", 22), fill=RED)
        d.text((r[0], r[1] + 140), "Invoice INV-2041   ·   Acme Design Studio   ·   $249.00 USD   ·   Visa •••• 4242", font=font("sans", 18), fill=(0, 0, 0))
        d.text((r[0], r[1] + 180), "Attempted Oct 3, 2026 9:41 AM — The customer's bank declined the charge. Ask for another payment method.", font=font("sans", 16), fill=(90, 90, 90))
        d.rectangle((r[0], r[1] + 220, r[0] + 200, r[1] + 264), fill=BLUE)
        d.text((r[0] + 40, r[1] + 232), "Retry payment", font=font("sans-b", 17), fill=(255, 255, 255))
    shot(path, "dashboard.payments.example.com — Invoices", body)


def shot_python_traceback(path):
    lines = ["$ python train.py --epochs 10",
             "Traceback (most recent call last):",
             "  File \"/Users/me/project/train.py\", line 3, in <module>",
             "    import torch",
             "ModuleNotFoundError: No module named 'torch'",
             "$ pip install torch",
             "error: externally-managed-environment"]

    def body(d, r):
        for i, line in enumerate(lines):
            color = (255, 80, 68) if ("Error" in line or "error" in line) else (230, 230, 230)
            d.text((r[0], r[1] + 20 + i * 34), line, font=font("mono", 22), fill=color)
    shot(path, "zsh — 120×40", body, bg=(26, 26, 26), chrome=(56, 56, 58))


def shot_flight_booking(path):
    def body(d, r):
        d.text((r[0], r[1]), "Booking confirmed ✈", font=font("sans-b", 30), fill=(0, 0, 0))
        d.rectangle((r[0], r[1] + 50, r[0] + int((r[2] - r[0]) * 0.7), r[1] + 270), fill=(237, 245, 255))
        d.text((r[0] + 30, r[1] + 70), "San Francisco (SFO)  →  Tokyo Haneda (HND)", font=font("sans-b", 26), fill=(0, 0, 0))
        d.text((r[0] + 30, r[1] + 120), "Flight NH 7   ·   Departs Fri, Nov 14 at 14:30   ·   Arrives Sat 18:55", font=font("sans", 19), fill=(0, 0, 0))
        d.text((r[0] + 30, r[1] + 160), "Passenger: Alex Doe   ·   Seat 32A   ·   Gate 112   ·   Confirmation code: K7Q2LM", font=font("sans", 19), fill=(0, 0, 0))
        d.text((r[0] + 30, r[1] + 210), "Check-in opens 24 hours before departure.", font=font("sans", 16), fill=(90, 90, 90))
    shot(path, "Mail — Your booking is confirmed", body)


def shot_chat_meeting(path):
    msgs = [("Sam", "Can we move the design review to Thursday? I'm out sick tomorrow."),
            ("Priya", "Sure, Thursday 3pm works for me. I'll update the calendar invite."),
            ("Sam", "Thanks! I'll also share the new onboarding mockups before the meeting.")]

    def body(d, r):
        for i, (who, msg) in enumerate(msgs):
            y = r[1] + 20 + i * 110
            x = r[0]
            d.ellipse((x, y, x + 44, y + 44), fill=(ORANGE, PURPLE, ORANGE)[i])
            d.text((x + 60, y), who, font=font("sans-b", 18), fill=(0, 0, 0))
            d.text((x + 60, y + 26), msg, font=font("sans", 19), fill=(0, 0, 0))
    shot(path, "#design-team", body)


def shot_weather(path):
    def body(d, r):
        d.text((r[0], r[1]), "London", font=font("sans-b", 40), fill=(0, 0, 0))
        d.text((r[0], r[1] + 60), "18° · Heavy rain ☂ · 90% chance of rain", font=font("sans", 28), fill=(0, 0, 0))
        for i, day in enumerate(["Mon ☔ 15°", "Tue ☔ 14°", "Wed ⛈ 13°", "Thu 🌧 16°", "Fri ☁ 17°"]):
            x = r[0] + i * 220
            d.rectangle((x, r[1] + 120, x + 200, r[1] + 260), fill=(255, 255, 255))
            d.text((x + 30, r[1] + 170), day, font=font("sans", 24), fill=(0, 0, 0))
    shot(path, "Weather", body, bg=(217, 230, 245))


def shot_swift_code(path):
    code = ["import Accelerate", "",
            "struct SearchIndex {",
            "    var vectors: [Float]",
            "    let dimension = 768",
            "",
            "    func search(query: [Float], topK: Int) -> [Int] {",
            "        // cosine similarity via matrix-vector product",
            "        var scores = [Float](repeating: 0, count: vectors.count / dimension)",
            "        cblas_sgemv(CblasRowMajor, CblasNoTrans, ...)",
            "        return scores.indices.sorted { scores[$0] > scores[$1] }.prefix(topK).map { $0 }",
            "    }", "}"]

    def body(d, r):
        for i, line in enumerate(code):
            color = (48, 220, 110) if "//" in line else ((255, 100, 160) if any(k in line for k in ("struct", "func", "import")) else (230, 230, 230))
            d.text((r[0], r[1] + 16 + i * 30), line, font=font("mono", 19), fill=color)
    shot(path, "SearchIndex.swift — Editor", body, bg=(31, 31, 31), chrome=(51, 51, 51))


# MARK: docs (make_docs.swift, one topic per page)

ANNUAL = [
    ("Quarterly business review",
     "Revenue grew 20% compared to last year, driven by strong sales in Europe and a new enterprise plan. "
     "Operating costs stayed flat, and the team hired four engineers. Next quarter we will focus on retention "
     "and on expanding the partner program in Japan."),
    ("Hiking trip to Mount Fuji",
     "We started the climb from the Yoshida trail at the fifth station before sunrise. The path was rocky and "
     "cold near the summit, but the view of the clouds below the crater was unforgettable. Pack warm gloves, "
     "a headlamp and enough water; the mountain huts sell instant noodles."),
    ("Sourdough bread recipe",
     "Feed the starter the night before. Mix 500 g flour, 350 g water and 100 g active starter, rest for an "
     "hour, then add 10 g salt. Stretch and fold four times, shape a boule, proof overnight in the fridge and "
     "bake in a Dutch oven at 250 °C."),
]

RECEIPT = ["BLUE BOTTLE COFFEE", "Hayes Valley, San Francisco", "", "Oat latte            5.50",
           "Almond croissant     4.75", "Cold brew            4.25", "", "TOTAL               14.50",
           "VISA **** 4242", "Thank you for visiting!"]


def text_pdf(path, pages, font_name="Helvetica", style="", size=13, title_size=22):
    pdf = FPDF(unit="pt", format=(612, 792))
    pdf.set_auto_page_break(False)
    for title, body in pages:
        pdf.add_page()
        pdf.set_font(font_name, style, title_size)
        pdf.set_xy(60, 42)
        pdf.multi_cell(492, 26, title)
        pdf.set_font(font_name, "", size)
        pdf.set_xy(60, 92)
        pdf.multi_cell(492, 21, body)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    pdf.output(path)


def scan_pdf(path):
    page = Image.new("RGB", (900, 1200), (247, 247, 247))
    d = ImageDraw.Draw(page)
    f = ImageFont.truetype(FONTS["mono"], 34)
    for i, line in enumerate(RECEIPT):
        d.text((120, 60 + i * 60), line, font=(ImageFont.truetype(FONTS["sans-b"], 34) if i == 0 else f), fill=(0, 0, 0))
    raster = os.path.join(os.path.dirname(path), ".receipt-tmp.jpg")
    page.save(raster, "JPEG", quality=90)
    pdf = FPDF(unit="pt", format=(612, 792))
    pdf.add_page()
    pdf.image(raster, x=40, y=40, w=532, h=712)
    pdf.output(path)
    os.remove(raster)


DOCX_XML = """<?xml version="1.0" encoding="UTF-16" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"""

DOCX_CT = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"""

DOCX_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"""


def docx(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", DOCX_CT)
        z.writestr("_rels/.rels", DOCX_RELS)
        z.writestr("word/document.xml", DOCX_XML.format(text=text))


def rtf(path, text):
    # \uc0\uNNNN: the Unicode runs every RTF reader (both engines') understands.
    body = "".join(ch if 32 <= ord(ch) < 127 else f"\\u{ord(ch)}? " for ch in text)
    write(path, "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Helvetica;}}\\uc0\\f0\\fs28 " + body.replace("\n", "\\par\n") + "}")


# MARK: long files (make_long.swift, byte-for-byte)

seed = 20251009


def pick(items):
    global seed
    seed = (seed * 6364136223846793005 + 1442695040888963407) & ((1 << 64) - 1)
    return items[(seed >> 33) % len(items)]


def pick_rev(items):
    return pick(list(reversed(items)))


PORTS = ["Falmouth", "Fowey", "Plymouth", "Salcombe", "Dartmouth", "Weymouth", "Poole", "Yarmouth",
         "Cherbourg", "St Peter Port", "Alderney", "Brixham", "Penzance", "Newlyn", "St Mary's"]
WINDS = ["northwest 3", "west 4", "southwest 4 to 5", "south 3", "variable 2", "northeast 4",
         "west 5, gusting 6", "southwest 3"]
SEAS = ["slight", "moderate", "smooth", "slight to moderate", "a long swell from the west"]
WEATHER = ["Sunny spells and good visibility.", "Overcast, with drizzle in the afternoon.",
           "Clear skies, cold in the evening.", "Showers passing through, rainbow at noon.",
           "Grey and still all morning, brighter later.", "High cloud and a halo around the sun."]
DAYS = ["Two reefs in the main until the wind eased, then full sail and a long beat to windward.",
        "Practised man-overboard drills with the fender twice; both recoveries under two minutes.",
        "Checked the rigging at the mast foot and tightened a loose shackle on the kicker.",
        "Long reach along the coast, logged a steady six knots most of the way.",
        "Motor-sailed the last hour to make the tide at the harbour entrance.",
        "Cleaned the bilge, topped up the water tanks and bought bread and fruit ashore.",
        "Took bearings on the lighthouse and the church spire to fix our position on the chart.",
        "Changed the genoa sheets and whipped the frayed ends with waxed twine.",
        "Quiet day at anchor: read, swam off the stern and cooked a curry for dinner."]
TIMES = ["06:10", "07:45", "08:30", "09:15", "10:40"]
MILES = ["18", "23", "27", "31", "36", "42"]
BAROS = ["1012", "1016", "1019", "1021", "1008"]

STANDOUTS = {
    41: "Day 41. The engine overheated off Start Point and stopped. The raw-water impeller had lost all its "
        "vanes, so we sailed the last eight miles into Brixham under genoa alone and fitted the spare impeller "
        "on the pontoon. Ordered two more from the chandlery; never leave harbour without a spare again.",
    66: "Day 66. At sunset a pod of a dozen common dolphins came racing in from the south and rode our bow "
        "wave for twenty minutes, leaping clear of the water and rolling to look up at us. Nobody wanted to go "
        "below for dinner. The skipper said it was the best evening of the whole season.",
    88: "Day 88. Picked up visitors' mooring ball M-417 in the Helford river, paid the harbour master eighteen "
        "pounds and rowed ashore for supplies. Quiet creek, oak woods down to the water, an egret fishing on the mud.",
    100: "Day 100. Fog came down so thick off the Lizard that we could not see the bow from the cockpit. We "
         "slowed to three knots, sounded the horn every two minutes and steered by radar and the plotter while a "
         "tanker's foghorn boomed somewhere to starboard. It lifted only as we reached the harbour wall.",
}


def log_page(number):
    from_ = pick(PORTS)
    to = pick([p for p in PORTS if p != from_])
    return (f"Day {number}. Left {from_} at {pick(TIMES)}, bound for {to}. "
            f"Wind {pick(WINDS)}, sea {pick(SEAS)}. {pick(WEATHER)} {pick(DAYS)} {pick_rev(DAYS)} "
            f"Moored in {to} after {pick(MILES)} miles. Barometer {pick(BAROS)} "
            "and steady. Crew well; the night watch went to the skipper and the mate in two-hour turns.")


def sailing_log(path):
    pages = [(f"Sailing log 2025 · page {n}", STANDOUTS.get(n) or log_page(n)) for n in range(1, 121)]
    text_pdf(path, pages, title_size=14)


TOPICS = ["the release checklist", "on-call handover", "the design review backlog", "hiring for the data team",
          "the customer survey results", "budget for the next quarter", "the support queue", "flaky tests in CI",
          "the onboarding guide", "the API deprecation plan", "laptop refresh", "the team offsite"]
ACTIONS = ["Priya will draft a proposal by Friday.", "Sam takes the follow-up with finance.",
           "We agreed to revisit this in two weeks.", "Ana will write it up in the wiki.",
           "No decision yet; collect more data first.", "Ben volunteered to own it this sprint.",
           "Moved to the next planning meeting.", "Done, closing the ticket."]
VERBS = ["discussed", "reviewed", "went through", "looked again at"]


def team_notes(path):
    global seed
    seed = 20251009  # make_long.swift reuses the one seed, continued where the log left off
    notes = "# Team notes 2026\n\n"
    week, told = 1, False
    while len(notes) < 150_000:
        notes += f"## Week {week}\n\n"
        if not told and len(notes) > 120_000:  # well past the 40 chunks (~64,000 characters) of the first step
            told = True
            notes += ("The big news: we agreed to move the whole team to a new office in Lisbon next March. "
                      "The rent is lower, half of the team already lives in Portugal, and the building has room "
                      "to grow. Marta will look at schools for the families who relocate, and the company covers "
                      "the moving costs.\n\n")
        for _ in range(6):
            notes += f"- On {pick(TOPICS)}: {pick(VERBS)} where it stands. {pick(ACTIONS)} {pick(ACTIONS)}\n"
        notes += "\n"
        week += 1
    write(path, notes)


def sensor_readings(path):
    rows = "timestamp,sensor,value\n"
    for row in range(12_000):
        rows += f"{1_760_000_000 + row * 60} {row % 17} {row * 7919 % 100_003}\n"
        if row == 11_500:
            rows += "4815162342 0 0\n"
    write(path, rows)


# MARK: Bengali and Arabic (make_lang.swift, notes verbatim)

BN = {
    "পরীক্ষার ফলাফল.txt": (
        "এসএসসি পরীক্ষার ফলাফল প্রকাশিত হয়েছে। এ বছর পাসের হার ৮৩ শতাংশ, যা গত বছরের চেয়ে বেশি। "
        "ঢাকা বোর্ডে মেয়েরা ছেলেদের চেয়ে ভালো ফল করেছে। ফলাফল শিক্ষা বোর্ডের ওয়েবসাইটে এবং মোবাইলে "
        "এসএমএসের মাধ্যমে জানা যাবে।"
    ),
    "ইলিশ মাছের রেসিপি.md": (
        "# সরিষা ইলিশ\n\nসরিষা ইলিশ রান্নার জন্য প্রথমে সরিষা বাটা, কাঁচা মরিচ, হলুদ আর লবণ দিয়ে ইলিশ "
        "মাছের টুকরোগুলো মাখিয়ে নিন। কড়াইয়ে সরিষার তেল গরম করে মাছগুলো দিন, ঢেকে দিয়ে কম আঁচে দশ মিনিট "
        "রান্না করুন। গরম ভাতের সঙ্গে পরিবেশন করুন।"
    ),
}
BN_PDF = [
    ("বাড়ি ভাড়ার চুক্তিপত্র",
     "ধানমন্ডি ৭ নম্বর রোডের তিন বেডরুমের ফ্ল্যাটটি মাসিক ২৫,০০০ টাকা ভাড়ায় দেওয়া হলো। ভাড়াটিয়া দুই মাসের "
     "ভাড়া অগ্রিম জমা দেবেন। প্রতি মাসের পাঁচ তারিখের মধ্যে ভাড়া পরিশোধ করতে হবে। বিদ্যুৎ, পানি ও গ্যাস বিল "
     "ভাড়াটিয়া আলাদাভাবে দেবেন। চুক্তির মেয়াদ এক বছর, এবং বাড়ি ছাড়ার আগে অন্তত দুই মাস আগে বাড়িওয়ালাকে "
     "লিখিতভাবে জানাতে হবে। ফ্ল্যাটে কোনো স্থায়ী পরিবর্তন করতে হলে বাড়িওয়ালার অনুমতি লাগবে।"),
    ("প্রোগ্রামিং ক্লাসের নোট",
     "আজকের ক্লাসে আমরা পাইথনে লুপ আর ফাংশন শিখেছি। for লুপ দিয়ে একটি তালিকার প্রতিটি উপাদানের উপর কাজ করা "
     "যায়, আর while লুপ চলতে থাকে যতক্ষণ শর্তটি সত্য থাকে। ফাংশন তৈরি করতে def শব্দটি ব্যবহার করা হয়, এবং "
     "return দিয়ে ফলাফল ফেরত পাঠানো হয়। বাড়ির কাজ: একটি ফাংশন লিখুন যা একটি সংখ্যার ফ্যাক্টোরিয়াল বের "
     "করে, তারপর লুপ দিয়ে এক থেকে দশ পর্যন্ত সব সংখ্যার ফ্যাক্টোরিয়াল ছাপুন।"),
]
BN_SUNDARBANS = (
    "সুন্দরবন ভ্রমণ\n\nখুলনা থেকে লঞ্চে করে আমরা তিন দিনের জন্য সুন্দরবনে গিয়েছিলাম। ম্যানগ্রোভ বনের খালে "
    "নৌকায় ঘুরে আমরা রয়েল বেঙ্গল টাইগারের পায়ের ছাপ দেখেছি। চিত্রা হরিণ, বানর আর একটি বড় কুমিরও চোখে "
    "পড়েছে। রাতে লঞ্চের ছাদে বসে তারা দেখা ছিল সবচেয়ে সুন্দর অভিজ্ঞতা।"
)

AR = {
    "وصفة الكبسة.txt": (
        "الكبسة السعودية: اغسل الأرز البسمتي وانقعه في الماء لمدة نصف ساعة. في قدر كبير، اقلِ البصل حتى يذبل "
        "ثم أضف قطع الدجاج والطماطم والثوم وبهارات الكبسة. أضف الماء واترك الدجاج ينضج، ثم أضف الأرز واطبخه على "
        "نار هادئة. قدّمها مع اللوز المحمص والزبيب."
    ),
    "Meeting minutes.rtf": (
        "محضر اجتماع فريق التطوير\n\nتقرر تأجيل إطلاق التطبيق إلى شهر ديسمبر بسبب مشاكل في نظام الدفع "
        "الإلكتروني. سيقوم فريق الخوادم بإصلاح الأخطاء قبل نهاية الشهر، وسيتم اختبار النسخة الجديدة مع مجموعة "
        "صغيرة من المستخدمين."
    ),
    "ملاحظة.md": (
        "# مُلاحَظَة\n\nقَالَ مُحَمَّدٌ إِنَّ الْمَكْتَبَةَ الْعَامَّةَ تُغْلَقُ فِي السَّاعَةِ الثَّامِنَةِ "
        "مَسَاءً يَوْمَ الْخَمِيسِ، فَلَا تَتَأَخَّرْ فِي إِعَادَةِ الْكُتُبِ."
    ),
}
AR_PDF = [
    ("التقرير الربعي",
     "ارتفعت المبيعات بنسبة خمسة عشر بالمئة في الربع الثالث مقارنة بالعام الماضي، بفضل نمو قوي في أسواق الخليج "
     "وإطلاق خدمة الاشتراك الجديدة. بقيت التكاليف التشغيلية مستقرة، وانضم ثلاثة مهندسين جدد إلى الفريق. في "
     "الربع القادم سنركز على رضا العملاء وتوسيع شبكة الشركاء."),
    ("رحلة إلى البتراء",
     "زرنا مدينة البتراء الأثرية في جنوب الأردن. مشينا عبر السيق الضيق حتى ظهرت الخزنة المنحوتة في الصخر "
     "الوردي. صعدنا إلى الدير في الصباح الباكر لتجنب الحر، وتناولنا الغداء في قرية وادي موسى. أنصح بارتداء "
     "حذاء مريح وحمل الكثير من الماء."),
]


def lang_pdf(path, pages, font_file, font_bold, name):
    pdf = FPDF(unit="pt", format=(612, 792))
    pdf.add_font(name, "", font_file)
    pdf.add_font(name, "B", font_bold)
    pdf.set_auto_page_break(False)
    for title, body in pages:
        pdf.add_page()
        pdf.set_font(name, "B", 22)
        pdf.set_xy(60, 42)
        pdf.multi_cell(492, 26, title, align="R")
        pdf.set_font(name, "", 14)
        pdf.set_xy(60, 92)
        pdf.multi_cell(492, 24, body, align="R")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    pdf.output(path)


def ar_visual(text):
    return get_display(arabic_reshaper.reshape(text))


def shot_wallet_bn(path):
    def body(d, r):
        d.text((r[0], r[1]), "টাকা পাঠান", font=font("bn-b", 34), fill=(0, 0, 0))
        d.rectangle((r[0], r[1] + 70, r[2], r[1] + 180), fill=(255, 230, 230))
        d.text((r[0] + 30, r[1] + 95), "⚠  লেনদেন ব্যর্থ হয়েছে: অপর্যাপ্ত ব্যালেন্স", font=font("bn-b", 30), fill=RED)
        d.text((r[0], r[1] + 195), "পরিমাণ: ৳ ৫,০০০   ·   প্রাপক: ০১৭০০-০০০০০০   ·   সময়: সকাল ১০:১২", font=font("bn", 24), fill=(0, 0, 0))
        d.text((r[0], r[1] + 250), "আপনার অ্যাকাউন্টে যথেষ্ট টাকা নেই। টাকা যোগ করে আবার চেষ্টা করুন।", font=font("bn", 22), fill=(90, 90, 90))
        d.rectangle((r[0], r[1] + 300, r[0] + 300, r[1] + 360), fill=PINK)
        d.text((r[0] + 50, r[1] + 315), "আবার চেষ্টা করুন", font=font("bn-b", 24), fill=(255, 255, 255))

    def titled(d, r):
        body(d, r)
    shot(path, "মোবাইল ওয়ালেট — টাকা পাঠান", body, title_font="bn")


def shot_payment_ar(path):
    def body(d, r):
        d.text((r[2] - 220, r[1]), ar_visual("إتمام الطلب"), font=font("ar-b", 34), fill=(0, 0, 0))
        d.rectangle((r[0], r[1] + 70, r[2], r[1] + 180), fill=(255, 230, 230))
        d.text((r[2] - 900, r[1] + 95), ar_visual("فشلت عملية الدفع: البطاقة مرفوضة من البنك  ⚠"), font=font("ar-b", 30), fill=RED)
        d.text((r[2] - 1000, r[1] + 195), ar_visual("المبلغ: ٣٥٠ ريال   ·   رقم الطلب: ٤٨٢١٧   ·   بطاقة فيزا تنتهي بـ ٤٢٤٢"), font=font("ar", 24), fill=(0, 0, 0))
        d.text((r[2] - 700, r[1] + 250), ar_visual("يرجى استخدام بطاقة أخرى أو المحاولة مرة أخرى لاحقاً."), font=font("ar", 22), fill=(90, 90, 90))
        d.rectangle((r[2] - 300, r[1] + 300, r[2], r[1] + 360), fill=BLUE)
        d.text((r[2] - 260, r[1] + 315), ar_visual("إعادة المحاولة"), font=font("ar-b", 24), fill=(255, 255, 255))
    shot(path, ar_visual("متجر إلكتروني — الدفع"), body, title_font="ar")


# MARK: speech (make_media.sh: `say -v Samantha` → espeak-ng)

def espeak_wav(espeak, voice, out_path, text):
    tmp = out_path + ".raw.wav"
    sh([espeak, "-v", voice, "-w", tmp, text])
    sh(["ffmpeg", "-loglevel", "error", "-y", "-i", tmp, "-ac", "1", "-ar", "16000", out_path])
    os.remove(tmp)


def build_media(tb, img, audio, espeak):
    M = os.path.join(tb, "synthetic")
    # Speech (verbatim sentences).
    for name, text in SPEECH.items():
        espeak_wav(espeak, "en-gb", os.path.join(audio, name), text)
    # System-sound stand-ins (not eval targets; the layout matches make_media.sh).
    for name, hz in (("sfx_Purr.wav", 740), ("sfx_Frog.wav", 240), ("sfx_Submarine.wav", 110), ("sfx_Glass.wav", 1975)):
        sh(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", f"sine=frequency={hz}:duration=1",
            "-ac", "1", "-ar", "16000", os.path.join(audio, name)])
    bench = os.path.join(audio, "bench_60s.wav")
    sh(["ffmpeg", "-loglevel", "error", "-y", "-stream_loop", "5", "-i", os.path.join(audio, "speech_cake_recipe.wav"),
        "-t", "60", bench])
    # Length-cap clips (make_media.sh's filters).
    trunc = os.path.join(M, "audio_trunc")
    sh(["ffmpeg", "-loglevel", "error", "-y", "-i", bench, "-t", "11", os.path.join(trunc, "bench_first11s.wav")])
    sh(["ffmpeg", "-loglevel", "error", "-y", "-i", bench, "-t", "30", os.path.join(trunc, "bench_first30s.wav")])
    sh(["ffmpeg", "-loglevel", "error", "-y", "-ss", "30", "-i", bench, "-t", "30", os.path.join(trunc, "bench_last30s.wav")])
    sh(["ffmpeg", "-loglevel", "error", "-y",
        "-i", os.path.join(audio, "speech_flight_delay.wav"), "-i", os.path.join(audio, "speech_flight_delay.wav"),
        "-f", "lavfi", "-t", "1", "-i", "anullsrc=r=16000:cl=mono",
        "-i", os.path.join(audio, "speech_revenue.wav"), "-i", os.path.join(audio, "speech_revenue.wav"),
        "-i", os.path.join(audio, "speech_revenue.wav"), "-i", os.path.join(audio, "speech_revenue.wav"),
        "-filter_complex", "[0][1][2][3][4][5][6]concat=n=7:v=0:a=1", "-ac", "1", "-ar", "16000",
        os.path.join(trunc, "flight_then_revenue_29s.wav")])
    # The 18 s film: penguin | zebra | parrot (6 s each), cut into moments (make_media.sh verbatim).
    V = os.path.join(M, "video")
    film = os.path.join(V, "penguin_zebra_parrot_18s.mp4")
    sh(["ffmpeg", "-loglevel", "error", "-y",
        "-loop", "1", "-t", "6", "-i", os.path.join(img, "Animals-Penguin.jpg"),
        "-loop", "1", "-t", "6", "-i", os.path.join(img, "Animals-Zebra.jpg"),
        "-loop", "1", "-t", "6", "-i", os.path.join(img, "Animals-Parrot.jpg"),
        "-filter_complex", "[0:v]scale=768:768,setsar=1[a];[1:v]scale=768:768,setsar=1[b];[2:v]scale=768:768,setsar=1[c];"
                            "[a][b][c]concat=n=3:v=1:a=0,format=yuv420p", "-r", "24", film])
    for s in (0, 6, 12):
        sh(["ffmpeg", "-loglevel", "error", "-y", "-ss", str(s), "-t", "6", "-i", film, "-c:v", "libx264",
            "-pix_fmt", "yuv420p", os.path.join(V, f"seg_{s}s.mp4")])
    sh(["ffmpeg", "-loglevel", "error", "-y", "-stream_loop", "2", "-i", film, "-t", "45", "-c", "copy",
        os.path.join(V, "bench_45s.mp4")])
    # A real MPEG-TS (the traps folder; the eval's "zoo clip" is the same film, remuxed).
    os.makedirs(os.path.join(tb, "traps", "real-mpeg-ts"), exist_ok=True)
    sh(["ffmpeg", "-loglevel", "error", "-y", "-i", film, "-c", "copy", "-f", "mpegts",
        os.path.join(tb, "traps", "real-mpeg-ts", "zoo clip.ts")])


def build(tb, img_cache, espeak, make_code):
    M = os.path.join(tb, "synthetic")
    img = os.path.join(M, "img")
    shots = os.path.join(M, "screenshots")
    audio = os.path.join(M, "audio")
    for folder in (img, shots, audio, os.path.join(M, "audio_trunc"), os.path.join(M, "video"),
                   os.path.join(M, "docs"), os.path.join(M, "long"), os.path.join(M, "lang", "bn"),
                   os.path.join(M, "lang", "ar"), os.path.join(tb, "traps"),
                   os.path.join(tb, "traps", "icons")):
        os.makedirs(folder, exist_ok=True)

    build_images(img_cache, img)

    shot_payment_error(os.path.join(shots, "shot_payment_error.png"))
    shot_python_traceback(os.path.join(shots, "shot_python_traceback.png"))
    shot_flight_booking(os.path.join(shots, "shot_flight_booking.png"))
    shot_chat_meeting(os.path.join(shots, "shot_chat_meeting.png"))
    shot_weather(os.path.join(shots, "shot_weather.png"))
    shot_swift_code(os.path.join(shots, "shot_swift_code.png"))

    build_media(tb, img, audio, espeak)

    # Docs (make_docs.swift).
    text_pdf(os.path.join(M, "docs", "Annual notes 2026.pdf"), ANNUAL)
    scan_pdf(os.path.join(M, "docs", "Scan 2026-09-14.pdf"))
    write(os.path.join(M, "docs", "Groceries.txt"),
          "Grocery list for the weekend: milk, eggs, bread, apples, coffee beans and dish soap.")
    write(os.path.join(M, "docs", "Tokyo itinerary.md"),
          "# Tokyo trip\n\nFlight to Tokyo Haneda on November 14. Hotel in Shinjuku for five nights. "
          "Day trip to Kamakura on Sunday to see the Great Buddha.")
    rtf(os.path.join(M, "docs", "Meeting notes.rtf"),
        "Design review moved to Thursday at 3pm because Sam was sick. Priya will update the calendar invite "
        "and Sam will share the onboarding mockups.")
    docx(os.path.join(M, "docs", "Vector search notes.docx"),
         "Notes on implementing cosine similarity search over embedding vectors with Accelerate in Swift: "
         "normalize vectors, use cblas_sgemv, keep the top-k with a heap.")

    # Long files (make_long.swift).
    sailing_log(os.path.join(M, "long", "Sailing log 2025.pdf"))
    team_notes(os.path.join(M, "long", "Team notes 2026.md"))
    sensor_readings(os.path.join(M, "long", "sensor readings.txt"))

    # Bengali (make_lang.swift).
    bn = os.path.join(M, "lang", "bn")
    for name, text in BN.items():
        write(os.path.join(bn, name), text)
    docx(os.path.join(bn, "Sundarbans trip.docx"), BN_SUNDARBANS)
    lang_pdf(os.path.join(bn, "বাড়ি ভাড়ার চুক্তি.pdf"), BN_PDF, FONTS["bn"], FONTS["bn-b"], "NotoBn")
    shot_wallet_bn(os.path.join(bn, "wallet_failed_bn.png"))
    espeak_wav(espeak, "bn", os.path.join(bn, "speech_weather_bn.wav"), BN_SPEECH)

    # Arabic.
    ar = os.path.join(M, "lang", "ar")
    for name, text in AR.items():
        path = os.path.join(ar, name)
        if name.endswith(".rtf"):
            rtf(path, text)
        else:
            write(path, text)
    lang_pdf(os.path.join(ar, "تقرير.pdf"), AR_PDF, FONTS["ar"], FONTS["ar-b"], "NotoAr")
    shot_payment_ar(os.path.join(ar, "payment_error_ar.png"))
    espeak_wav(espeak, "ar", os.path.join(ar, "speech_weather_ar.wav"), AR_SPEECH)

    # Traps (the make-testbed.sh list, Linux-style).
    traps = os.path.join(tb, "traps")
    copy(os.path.join(img, "Animals-Penguin.jpg"), os.path.join(traps, "duplicates", "penguin.jpg"))
    copy(os.path.join(img, "Animals-Penguin.jpg"), os.path.join(traps, "duplicates", "penguin copy.jpg"))
    names = os.path.join(traps, "Names with spaces & ünïcödé")
    copy(os.path.join(img, "Flowers-Sunflower.jpg"), os.path.join(names, "Café menü — 日本語.jpg"))
    copy(os.path.join(shots, "shot_chat_meeting.png"),
         os.path.join(names, "Screenshot 2026-10-05 at 9.41.07\u202fPM.png"))
    project = os.path.join(traps, "MyProject")
    write(os.path.join(project, "package.json"), '{ "name": "my-project" }\n')
    copy(os.path.join(img, "Animals-Owl.jpg"), os.path.join(project, "assets", "hero.jpg"))
    copy(os.path.join(img, "Animals-Eagle.jpg"), os.path.join(project, "node_modules", "lib", "logo.jpg"))
    copy(os.path.join(shots, "shot_weather.png"),
         os.path.join(project, "screenshots", "Screenshot 2026-10-02 at 8.15.40 AM.png"))
    # node_modules outside a project, a hidden folder, and an app bundle: all skipped.
    copy(os.path.join(img, "Nature-Leaf.jpg"), os.path.join(traps, "node_modules", "pkg", "leaf.jpg"))
    copy(os.path.join(img, "Nature-Zen.jpg"), os.path.join(traps, ".hidden", "zen.jpg"))
    copy(os.path.join(img, "Fun-Medal.jpg"), os.path.join(traps, "Fake.app", "Contents", "Resources", "medal.jpg"))
    # Icons below the 256 px minimum: skipped.
    for px in (32, 64, 128):
        im = Image.open(os.path.join(img, "Sports-Target.jpg")).convert("RGB")
        im.thumbnail((px, px))
        im.save(os.path.join(traps, "icons", f"target-{px}.png"))
    # A dataset: 600 machine-named clips in one folder -> the detector should flag it.
    clips = os.path.join(traps, "dataset_clips")
    os.makedirs(clips, exist_ok=True)
    sh(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
        "-ac", "1", "-ar", "16000", os.path.join(clips, "clip_000000.wav")])
    for i in range(1, 600):
        copy(os.path.join(clips, "clip_000000.wav"), os.path.join(clips, f"clip_{i:06d}.wav"))

    # Code (the same generator the macOS builder runs).
    subprocess.run([sys.executable, "-I", make_code, os.path.join(tb, "code")], check=True,
                   stdin=subprocess.DEVNULL)

    write(os.path.join(tb, "README.md"), (
        "# DigUp testbed (Linux twin)\n\nA sandbox for testing DigUp: the same layout and known content as\n"
        "scripts/make-testbed.sh, built with Linux tools (Wikimedia fixtures for the macOS system\n"
        "pictures, espeak-ng for `say`, ffmpeg, PIL, fpdf2). Delete it any time. Index it with\n"
        "`digup index <dir>` and `digup index <dir> --code`, then `digup eval evals/*.json\n"
        "--index <index folder>` with DIGUP_TESTBED=<dir>.\n"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--img-cache", default=None)
    ap.add_argument("--espeak", default=os.environ.get("ESPEAK_NG", "espeak-ng"))
    args = ap.parse_args()
    tb = os.path.abspath(args.dir)
    if os.path.exists(tb):
        sys.exit(f"{tb} already exists; delete it first")
    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(os.path.dirname(here))
    os.makedirs(tb, exist_ok=True)
    build(tb, args.img_cache or os.path.join(tb, ".img-cache"), args.espeak,
          os.path.join(repo, "scripts", "testbed", "make_code.py"))
    print(f"testbed in {tb}")


if __name__ == "__main__":
    main()
