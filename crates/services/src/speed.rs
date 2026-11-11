//! `digup speed --model`: how fast this machine runs the model, timed on a made-up text,
//! picture and sound next to the M4 Pro behind the fixed costs (`ModelSpeed.swift`). Linux and
//! Windows have no model-free check (the Mac's is Metal's), and a PC's processor can run the
//! model fifty times slower than the M4 Pro: their estimates go by this until indexing has
//! measured enough itself. Kept in `costs.json` beside the index, so it runs once.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use digup_runtime::prompts;
use digup_runtime::Embedder;

use crate::costs::Costs;

/// Seconds for each input on the M4 Pro (Metal, Q8_0), 2026-10-13: the best of three after a
/// first run, the same in two runs.
pub const BASELINE: (f64, f64, f64, f64, f64) = (0.026, 0.123, 0.265, 0.067, 0.220);

/// The inputs' baseline seconds by the `fields` names, for the human line.
pub fn baseline_fields() -> BTreeMap<String, f64> {
    BTreeMap::from([
        ("text".to_string(), BASELINE.0),
        ("picture".to_string(), BASELINE.1),
        ("screen".to_string(), BASELINE.2),
        ("frame".to_string(), BASELINE.3),
        ("sound".to_string(), BASELINE.4),
    ])
}

/// One embedding of each input: a page of text as a document, a picture at a photo's budget
/// (140 tokens), a screenshot's (280), a video frame's (70), and a 30 s window of sound. A
/// check times the text, the picture and the sound; a screenshot and a frame go as the picture
/// does (measured: within 7% of it on an i5-8350U, 20% on ARM), which spares a slow PC half the
/// check.
#[derive(Debug, Clone, Copy)]
pub struct Times {
    pub text: f64,
    pub picture: f64,
    pub screen: f64,
    pub frame: f64,
    pub sound: f64,
}

impl Times {
    /// From the three timed inputs, with the screenshot and the frame following the picture's
    /// ratio to the M4 Pro.
    pub fn from_three(text: f64, picture: f64, sound: f64) -> Self {
        let ratio = picture / BASELINE.1;
        Self {
            text,
            picture,
            screen: BASELINE.2 * ratio,
            frame: BASELINE.3 * ratio,
            sound,
        }
    }

    pub fn fields(&self) -> BTreeMap<String, f64> {
        BTreeMap::from([
            ("text".to_string(), self.text),
            ("picture".to_string(), self.picture),
            ("screen".to_string(), self.screen),
            ("frame".to_string(), self.frame),
            ("sound".to_string(), self.sound),
        ])
    }

    /// From `fields` (another process's measurement); None when a field is missing.
    pub fn from_fields(fields: &BTreeMap<String, f64>) -> Option<Self> {
        Some(Self {
            text: *fields.get("text")?,
            picture: *fields.get("picture")?,
            screen: *fields.get("screen")?,
            frame: *fields.get("frame")?,
            sound: *fields.get("sound")?,
        })
    }
}

/// What a check found: each input's best time, the vectors it made ("text", "picture",
/// "sound": for comparing the GPU's with the processor's), each try's seconds, whether its best
/// was seen twice, and the processors' worth of other programs' work while it timed.
#[derive(Debug, Clone)]
pub struct Measurement {
    pub times: Times,
    pub vectors: HashMap<String, Vec<f32>>,
    pub tries: Vec<f64>,
    pub steady: bool,
    pub others: f64,
}

impl Measurement {
    pub fn total(&self) -> f64 {
        self.times.text + self.times.picture + self.times.sound
    }
}

/// Two tries within this of each other agree.
pub const AGREE: f64 = 1.15;

/// About a page of plain prose (1,600 characters), as a document chunk is: the engine's
/// `page` repeats this sentence nine times.
const SENTENCE: &str = "The meeting moved to Thursday because the room was booked. Priya will send the notes and the budget draft; the receipts from the hardware store are in the shared folder. ";
pub fn page() -> String {
    SENTENCE.repeat(9)
}

/// A first run of each input (the backend loads, a GPU compiles its kernels), then tries of
/// all three, each input's best kept: at most `tries`, and fewer once the best try is matched
/// by another (a busy moment makes a try slower, never faster), or once two tries came out
/// clearly slower than `rival`'s total (the other device's, which is then kept whatever these
/// times are).
pub fn measure(
    model: &dyn Embedder,
    tries: i64,
    rival: Option<f64>,
) -> Result<Measurement, String> {
    let folder = std::env::temp_dir().join(format!("DigUp-speed-{}", uuid_like()));
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("can't make the speed check's folder: {e}"))?;
    let result = measure_in(model, tries, rival, &folder);
    let _ = std::fs::remove_dir_all(&folder);
    result
}

fn measure_in(
    model: &dyn Embedder,
    tries: i64,
    rival: Option<f64>,
    folder: &Path,
) -> Result<Measurement, String> {
    let picture = folder.join("picture.bmp");
    let sound = folder.join("sound.wav");
    std::fs::write(&picture, bitmap(960, 720))
        .map_err(|e| format!("can't write the picture: {e}"))?;
    std::fs::write(&sound, wave(30)).map_err(|e| format!("can't write the sound: {e}"))?;
    let text = prompts::document(Some("notes.txt"), &page());

    let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
    vectors.insert(
        "text".to_string(),
        model
            .embed_texts(std::slice::from_ref(&text))
            .map_err(|e| format!("the speed check's input failed: {e}"))?
            .into_iter()
            .next()
            .ok_or("the speed check's input failed: no text vector")?,
    );
    vectors.insert(
        "picture".to_string(),
        take(
            model
                .embed_images(&[picture.to_string_lossy().into_owned()], 140)
                .map_err(|e| e.to_string())?,
            "picture",
        )?,
    );
    vectors.insert(
        "sound".to_string(),
        take(
            model
                .embed_audio(&[sound.to_string_lossy().into_owned()])
                .map_err(|e| e.to_string())?,
            "sound",
        )?,
    );

    let mut best = (f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut totals: Vec<f64> = Vec::new();
    let mut others: Vec<f64> = Vec::new();
    for _ in 0..tries.max(1) {
        let before = crate::system_load::sample();
        let seconds = (
            timed(|| {
                model
                    .embed_texts(std::slice::from_ref(&text))
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })?,
            timed(|| {
                take(
                    model
                        .embed_images(&[picture.to_string_lossy().into_owned()], 140)
                        .map_err(|e| e.to_string())?,
                    "picture",
                )?;
                Ok(())
            })?,
            timed(|| {
                take(
                    model
                        .embed_audio(&[sound.to_string_lossy().into_owned()])
                        .map_err(|e| e.to_string())?,
                    "sound",
                )?;
                Ok(())
            })?,
        );
        if let Some(before) = before {
            if let Some(after) = crate::system_load::sample() {
                others.push(crate::system_load::others(&before, &after));
            }
        }
        best = (
            best.0.min(seconds.0),
            best.1.min(seconds.1),
            best.2.min(seconds.2),
        );
        totals.push(seconds.0 + seconds.1 + seconds.2);
        if matched(&totals) {
            break;
        }
        if let Some(rival) = rival {
            if totals.iter().filter(|total| **total > rival * 1.3).count() >= 2 {
                break;
            }
        }
    }
    Ok(Measurement {
        steady: matched(&totals),
        times: Times::from_three(best.0, best.1, best.2),
        vectors,
        tries: totals,
        others: if others.is_empty() {
            0.0
        } else {
            others.iter().sum::<f64>() / others.len() as f64
        },
    })
}

/// A made-up file's embedding result, or the engine's error line.
fn take(result: Vec<digup_runtime::Embedding>, what: &str) -> Result<Vec<f32>, String> {
    let first = result
        .into_iter()
        .next()
        .unwrap_or(digup_runtime::Embedding::failed(format!("no {what} input")));
    match first.vector {
        Some(vector) => Ok(vector),
        None => Err(format!(
            "the speed check's input failed: {}",
            first.error.unwrap_or_default()
        )),
    }
}

fn timed(work: impl Fn() -> Result<(), String>) -> Result<f64, String> {
    let started = Instant::now();
    work()?;
    Ok(started.elapsed().as_secs_f64())
}

/// `ModelSpeed.matched()`: the best try has been seen (nearly) twice.
fn matched(totals: &[f64]) -> bool {
    if totals.is_empty() {
        return false;
    }
    let fastest = totals.iter().fold(f64::INFINITY, |a, &b| a.min(b));
    totals
        .iter()
        .filter(|total| **total <= fastest * AGREE)
        .count()
        >= 2
}

/// How far two runs' vectors agree: the least cosine over the inputs both made (0 when one is
/// missing).
pub fn agreement(one: &HashMap<String, Vec<f32>>, other: &HashMap<String, Vec<f32>>) -> f64 {
    ["text", "picture", "sound"]
        .map(|name| {
            let (a, b) = match (one.get(name), other.get(name)) {
                (Some(a), Some(b)) if !a.is_empty() && a.len() == b.len() => (a, b),
                _ => return 0.0,
            };
            let dot: f64 = a
                .iter()
                .zip(b)
                .map(|(x, y)| f64::from(*x) * f64::from(*y))
                .sum();
            let norms = a
                .iter()
                .map(|x| f64::from(*x) * f64::from(*x))
                .sum::<f64>()
                .sqrt()
                * b.iter()
                    .map(|y| f64::from(*y) * f64::from(*y))
                    .sum::<f64>()
                    .sqrt();
            if norms > 0.0 {
                dot / norms
            } else {
                0.0
            }
        })
        .into_iter()
        .fold(f64::INFINITY, f64::min)
}

/// Each of `Costs`' fields as this machine compares. A cost's share that is the model's work
/// goes as this machine ran the model (pages of text, documents and code as the text did,
/// pictures as the picture, sound as the sound); the rest (reading files, decoding) stays as on
/// the M4 Pro, since a slow processor slows it far less. A screenshot here is a picture at the
/// screenshot's budget with no OCR (OCR sets the M4 Pro's pace; Linux and Windows have none
/// yet). A second of video is its keyframes' share and its soundtrack's.
pub fn factors(times: &Times) -> BTreeMap<String, f64> {
    let costs = Costs::default();
    let (base_text, base_picture, _base_screen, base_frame, base_sound) = BASELINE;
    let scaled = |cost: f64, model: f64, here: f64| -> f64 {
        let part = cost.min(model);
        (cost - part + part * here / model) / cost
    };
    let window = 25.0; // seconds of sound a 30 s window stands for (a window every 25 s)
    let soundtrack = costs.audio_second;
    let frames = costs.video_second - soundtrack;
    let video = frames * times.frame / base_frame
        + soundtrack * scaled(soundtrack, base_sound / window, times.sound / window);
    BTreeMap::from([
        (
            "image".to_string(),
            scaled(costs.image, base_picture, times.picture),
        ),
        (
            // A screenshot here is a picture at the screenshot's budget with no OCR.
            "screenshot".to_string(),
            ((costs.image - base_picture).max(0.0) + times.screen) / costs.screenshot,
        ),
        (
            "pdfPage".to_string(),
            scaled(costs.pdf_page, base_text, times.text),
        ),
        (
            "docChunk".to_string(),
            scaled(costs.doc_chunk, base_text, times.text),
        ),
        (
            "codeStretch".to_string(),
            scaled(costs.code_stretch, base_text, times.text),
        ),
        (
            "audioSecond".to_string(),
            scaled(
                costs.audio_second,
                base_sound / window,
                times.sound / window,
            ),
        ),
        ("videoSecond".to_string(), video / costs.video_second),
    ])
}

/// A pseudo-UUID for the temp folder's name (the folder is deleted; nothing reads it).
fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{:x}", std::process::id(), nanos)
}

/// A 24-bit BMP of soft gradients and stripes (any picture fills its budget's tokens the same).
pub fn bitmap(width: usize, height: usize) -> Vec<u8> {
    let row = (width * 3).div_ceil(4) * 4;
    let mut data: Vec<u8> = Vec::with_capacity(54 + row * height);
    let put = |data: &mut Vec<u8>, value: u32, bytes: usize| {
        for index in 0..bytes {
            data.push(((value >> (8 * index)) & 0xFF) as u8);
        }
    };
    data.extend(b"BM");
    put(&mut data, 54 + (row * height) as u32, 4);
    put(&mut data, 0, 4);
    put(&mut data, 54, 4);
    put(&mut data, 40, 4);
    put(&mut data, width as u32, 4);
    put(&mut data, height as u32, 4);
    put(&mut data, 1, 2);
    put(&mut data, 24, 2);
    put(&mut data, 0, 4);
    put(&mut data, (row * height) as u32, 4);
    put(&mut data, 2835, 4);
    put(&mut data, 2835, 4);
    put(&mut data, 0, 4);
    put(&mut data, 0, 4);
    for y in 0..height {
        for x in 0..width {
            data.push(((x * 255) / width) as u8);
            data.push(((y * 255) / height) as u8);
            data.push(if (x / 40 + y / 40) % 2 == 0 { 200 } else { 60 });
        }
        data.extend(std::iter::repeat_n(0, row - width * 3)); // the row padded to a 4-byte multiple, as the header says
    }
    data
}

/// A 16 kHz mono 16-bit WAV of rising and falling tones with a little noise.
pub fn wave(seconds: usize) -> Vec<u8> {
    let rate = 16000usize;
    let count = rate * seconds;
    let mut data: Vec<u8> = Vec::with_capacity(44 + count * 2);
    let put = |data: &mut Vec<u8>, value: u32, bytes: usize| {
        for index in 0..bytes {
            data.push(((value >> (8 * index)) & 0xFF) as u8);
        }
    };
    data.extend(b"RIFF");
    put(&mut data, 36 + (count * 2) as u32, 4);
    data.extend(b"WAVEfmt ");
    put(&mut data, 16, 4);
    put(&mut data, 1, 2);
    put(&mut data, 1, 2);
    put(&mut data, rate as u32, 4);
    put(&mut data, (rate * 2) as u32, 4);
    put(&mut data, 2, 2);
    put(&mut data, 16, 2);
    data.extend(b"data");
    put(&mut data, (count * 2) as u32, 4);
    let mut noise: u32 = 12345;
    for index in 0..count {
        let time = index as f64 / rate as f64;
        let pitch = 220.0 + 180.0 * (time * 0.7).sin();
        noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let value = 0.3 * (2.0 * std::f64::consts::PI * pitch * time).sin()
            + 0.05 * ((noise >> 16) as f64 / 65536.0 - 0.5);
        // Swift's `Int16(max(-1, min(1, value)) * 32000)` truncates toward zero.
        let sample = (value.clamp(-1.0, 1.0) * 32000.0).trunc() as i16;
        data.push((sample as u16 & 0xFF) as u8);
        data.push(((sample as u16) >> 8) as u8);
    }
    data
}

/// The speed check's folder path, for the "kept in …" line.
pub fn kept_in(folder: &Path) -> PathBuf {
    folder.join("costs.json")
}
