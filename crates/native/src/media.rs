//! Audio and video through ffmpeg/ffprobe — `Platform/Portable/Media+Portable.swift`'s Rust
//! side: `MediaInfo.duration/streams`, `AudioExtractor.windows`, `VideoExtractor.keyframes`
//! and `VideoExtractor.frame(at:of:pixels:to:)`.
//!
//! The tools are the engine's: `crates/native/src/ffmpeg.rs` found them (`$DIGUP_FFMPEG`,
//! beside the executable, PATH) and runs them the engine's way (argv arrays, stderr to the
//! null device, 64 KB reads, the reader's refusal ends the tool). Decoding is ffmpeg's, never
//! a Rust codec's: the windows the model embeds and the frames it sees must be the exact
//! bytes the engine writes.

use std::io::Write;
use std::path::{Path, PathBuf};

use digup_core::extract::ExtractError;
use digup_core::jsonline;
use digup_core::rules;
use digup_core::uuid::uuid_string;

use crate::ffmpeg;
use crate::rgb::RgbImage;

/// `AudioExtractor.sampleRate` — everything the model hears is 16 kHz mono.
pub const SAMPLE_RATE: f64 = 16_000.0;

/// `MediaPiece`: a stretch of audio written to a temporary WAV for the model, or a video
/// frame written to a JPEG.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaPiece {
    pub path: PathBuf,
    pub start: f64,
    pub end: f64,
}

/// Foundation spells a double in a string interpolation the way `jsonline` spells it in JSON,
/// keeping the `.0` on a whole value ("3.0", "0.0") that JSON drops. Refusal texts carry
/// interpolated seconds, so they get this form.
fn swift_interpolated(value: f64) -> String {
    match jsonline::swift_double(value) {
        Some(text) if text.contains('.') || text.contains('e') => text,
        Some(text) => format!("{text}.0"),
        None => format!("{value}"), // nan/±∞: Foundation spells them "nan"/"inf"; Rust's own spelling stands in
    }
}

/// `MediaInfo.duration(of:)`: ffprobe's format duration; nil when it can't say a finite
/// positive number.
pub fn duration_of(url: &Path) -> Option<f64> {
    let text = ffmpeg::output(
        ffmpeg::ffprobe().as_deref(),
        &[
            "-v".into(),
            "error".into(),
            "-show_entries".into(),
            "format=duration".into(),
            "-of".into(),
            "default=noprint_wrappers=1:nokey=1".into(),
            url.to_string_lossy().into_owned(),
        ],
    )
    .ok()?;
    let seconds: f64 = text.trim().parse().ok()?;
    (seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

/// `MediaInfo.streams(of:)`: which kinds of streams the file has.
pub fn streams_of(url: &Path) -> (bool, bool) {
    let Ok(text) = ffmpeg::output(
        ffmpeg::ffprobe().as_deref(),
        &[
            "-v".into(),
            "error".into(),
            "-show_entries".into(),
            "stream=codec_type".into(),
            "-of".into(),
            "default=noprint_wrappers=1:nokey=1".into(),
            url.to_string_lossy().into_owned(),
        ],
    ) else {
        return (false, false);
    };
    (text.contains("audio"), text.contains("video"))
}

/// `AudioExtractor.rms`: the window's loudness; > 0.003 is ≈ −50 dBFS.
pub fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0f64;
    for sample in samples {
        let value = f64::from(*sample) / 32768.0;
        sum += value * value;
    }
    (sum / samples.len() as f64).sqrt()
}

/// `AudioExtractor.writeWAV`: the 44-byte canonical header and the Int16 samples,
/// little-endian (`samples.withUnsafeBytes` over the Int16 buffer).
pub fn write_wav(samples: &[i16], url: &Path) -> Result<(), ExtractError> {
    let name = || {
        url.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut data: Vec<u8> = Vec::with_capacity(44 + samples.len() * 2);
    let rate = SAMPLE_RATE as u32;
    data.extend(b"RIFF");
    data.extend((36u32 + samples.len() as u32 * 2).to_le_bytes());
    data.extend(b"WAVEfmt ");
    data.extend(16u32.to_le_bytes());
    data.extend(1u16.to_le_bytes()); // PCM
    data.extend(1u16.to_le_bytes()); // mono
    data.extend(rate.to_le_bytes());
    data.extend((rate * 2).to_le_bytes());
    data.extend(2u16.to_le_bytes());
    data.extend(16u16.to_le_bytes());
    data.extend(b"data");
    data.extend((samples.len() as u32 * 2).to_le_bytes());
    for sample in samples {
        data.extend(sample.to_le_bytes());
    }
    let file = std::fs::File::create(url);
    let written = file.and_then(|mut file| file.write_all(&data));
    written.map_err(|_| ExtractError::Unreadable {
        why: format!("can't write {}", name()),
    })
}

/// The streaming state of `AudioExtractor.windows`: the sample buffer, whose consumed
/// prefix `drain_hop` removes exactly as the engine's `buffer.removeFirst(hopSamples)`
/// does, the odd byte carried between reads, and the sample offset of the buffer's first
/// sample (the engine's `offset`, naming `buffer[0]` in the file).
struct Windows {
    buffer: Vec<i16>,
    offset: i64,
    window_samples: usize,
    hop_samples: usize,
    pieces: Vec<MediaPiece>,
    odd: Option<u8>,
}

impl Windows {
    fn emit(&mut self, count: usize, folder: &Path) -> Result<(), ExtractError> {
        let slice = &self.buffer[..count];
        // The negated form is the Swift text (`guard !(rms > 0.003)`): true for NaN too,
        // which `rms <= 0.003` would not be.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        if !(rms(slice) > 0.003) {
            return Ok(()); // ≈ −50 dBFS: silence isn't worth a vector
        }
        let out = folder.join(format!("{}.wav", uuid_string()));
        write_wav(slice, &out)?;
        self.pieces.push(MediaPiece {
            path: out,
            start: self.offset as f64 / SAMPLE_RATE,
            end: (self.offset + count as i64) as f64 / SAMPLE_RATE,
        });
        Ok(())
    }

    /// The read callback: carry the odd byte, spell the pairs as little-endian Int16, and cut
    /// every whole window the buffer now holds.
    fn feed(&mut self, data: &[u8], folder: &Path) -> Result<(), ExtractError> {
        // The fresh `bytes` Vec per read is Swift's OWN shape: the engine's callback starts
        // `var bytes: [UInt8] = []` + `reserveCapacity(data.count + 1)` for every pipe read
        // (`Media+Portable.swift:112-115`), prepending the odd byte the same way. A
        // reused scratch buffer would be CHEAPER than the engine and would drift the
        // append arithmetic away from the twin; the allocation is the parity, not sloppiness.
        let mut bytes: Vec<u8> = Vec::with_capacity(data.len() + 1);
        if let Some(first) = self.odd.take() {
            bytes.push(first);
        }
        bytes.extend_from_slice(data);
        if bytes.len() % 2 == 1 {
            self.odd = bytes.pop();
        }
        self.buffer.reserve(bytes.len() / 2);
        for pair in bytes.as_chunks::<2>().0 {
            self.buffer.push(i16::from_le_bytes([pair[0], pair[1]]));
        }
        while self.buffer.len() >= self.window_samples {
            self.emit(self.window_samples, folder)?;
            self.drain_hop();
        }
        Ok(())
    }

    /// `buffer.removeFirst(hopSamples)` (Media+Portable.swift:116): the samples a window
    /// consumed leave the buffer, so the stream's memory is one window even for a 3-hour
    /// podcast. Keeping a head index INSTEAD grows the Vec with the whole file — a 3-hour
    /// podcast at 16 kHz mono is 345 M samples ≈ 690 MB, past the watchdog's 4–8 GB cap on
    /// a loaded machine, and the Vec's doubling spikes it further. `drain` keeps the
    /// capacity, which is what the engine's own ArraySlice removal costs too.
    fn drain_hop(&mut self) {
        self.buffer.drain(..self.hop_samples).for_each(|_| ());
        self.offset += self.hop_samples as i64;
    }
}

/// `AudioExtractor.windows` (`Media+Portable.swift:91-131`): decode a file's (or a video's
/// soundtrack) audio to 16 kHz mono through ffmpeg, write `window`-second WAV pieces `hop`
/// seconds apart, skipping silent ones; memory stays at one window even for a 3-hour
/// podcast. The tail covers new audio past the previous window's overlap, the whole file if
/// it was shorter than a window.
pub fn windows(
    url: &Path,
    window: f64,
    hop: f64,
    minimum: f64,
    folder: &Path,
) -> Result<(Vec<MediaPiece>, f64), ExtractError> {
    if !streams_of(url).0 {
        return Ok((Vec::new(), 0.0));
    }
    let duration = duration_of(url).unwrap_or(0.0);
    let mut state = Windows {
        buffer: Vec::new(),
        offset: 0,
        window_samples: (window * SAMPLE_RATE) as usize,
        hop_samples: (hop * SAMPLE_RATE) as usize,
        pieces: Vec::new(),
        odd: None,
    };
    let arguments: Vec<String> = [
        "-v".to_string(),
        "error".into(),
        "-nostdin".into(),
        "-i".into(),
        url.to_string_lossy().into_owned(),
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        (SAMPLE_RATE as i32).to_string(),
        "-f".into(),
        "s16le".into(),
        "-".into(),
    ]
    .to_vec();
    ffmpeg::run(ffmpeg::ffmpeg().as_deref(), &arguments, &mut |data| {
        state.feed(data, folder)
    })
    .map_err(|error| error.extract())?;
    // The tail: new audio beyond the previous window's overlap, or the whole file if it was
    // shorter than a window.
    let count = state.buffer.len();
    let already_covered: i64 = if state.offset == 0 {
        0
    } else {
        (state.window_samples - state.hop_samples) as i64
    };
    if count as i64 - already_covered >= (minimum * SAMPLE_RATE) as i64 {
        state.emit(count, folder)?;
    }
    Ok((
        state.pieces,
        if duration > 0.0 {
            duration
        } else {
            (state.offset + count as i64) as f64 / SAMPLE_RATE
        },
    ))
}

/// `VideoExtractor.keyframes` (`Media+Portable.swift:137-176`): sample a frame every
/// `interval` seconds through ffmpeg (`fps=…:round=up` gives each slot the last frame at or
/// before it) and keep one per shot — a frame whose 8×8 average hash is within 4 bits of the
/// last kept one only extends it, and its file goes. Slots past the end go too. Each kept
/// frame ends where the next kept frame starts.
pub fn keyframes(
    url: &Path,
    interval: f64,
    max_pixel: usize,
    max_frames: usize,
    folder: &Path,
) -> Result<(Vec<MediaPiece>, f64, bool), ExtractError> {
    let streams = streams_of(url);
    let duration = duration_of(url).unwrap_or(0.0);
    if !(duration > 0.0 && streams.1) {
        return Ok((Vec::new(), duration, streams.0));
    }
    let step = interval.max(duration / max_frames as f64);
    let first = 0.5f64.min(duration / 2.0);
    let prefix = format!("frame-{}-", uuid_string());
    // Every frame is decoded anyway: reading from the start costs nothing over seeking, and
    // an MPEG-TS has no index for a seek.
    let filter = format!(
        "fps=fps={}:start_time={}:round=up,scale={}:{}:force_original_aspect_ratio=decrease",
        swift_interpolated(1.0 / step),
        swift_interpolated(first),
        max_pixel,
        max_pixel
    );
    let arguments: Vec<String> = [
        "-v".to_string(),
        "error".into(),
        "-nostdin".into(),
        "-i".into(),
        url.to_string_lossy().into_owned(),
        "-an".into(),
        "-vf".into(),
        filter,
        "-q:v".into(),
        "3".into(),
        "-f".into(),
        "image2".into(),
        folder
            .join(format!("{prefix}%06d.jpg"))
            .to_string_lossy()
            .into_owned(),
    ]
    .to_vec();
    ffmpeg::run(ffmpeg::ffmpeg().as_deref(), &arguments, &mut |_| Ok(()))
        .map_err(|error| error.extract())?;
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map_err(|_| ExtractError::Unreadable {
            why: "can't list the frame folder".into(),
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&prefix))
        .collect();
    names.sort();
    let mut frames: Vec<MediaPiece> = Vec::new();
    let mut last_hash: Option<u64> = None;
    for (index, name) in names.iter().enumerate() {
        let path = folder.join(name);
        let start = first + index as f64 * step;
        // The negated form is the Swift text (`guard !(start < duration)`): true for NaN
        // too, which `start >= duration` would not be.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        if !(start < duration) {
            // fps can fill a slot past the end.
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if let Some(image) = RgbImage::load(&path) {
            let hash = image.average_hash();
            if let Some(last) = last_hash {
                if (hash ^ last).count_ones() <= 4 {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            }
            last_hash = Some(hash);
        }
        frames.push(MediaPiece {
            path,
            start,
            end: duration,
        });
    }
    // Each kept frame stands for the stretch until the next kept frame.
    for index in 0..frames.len().saturating_sub(1) {
        frames[index].end = frames[index + 1].start;
    }
    Ok((frames, duration, streams.0))
}

/// `VideoExtractor.frame(at:of:pixels:to:)` (`Media+Portable.swift:179-198`): the frame shown
/// at `seconds` as a JPEG preview, long side at most `pixels`. Seeking before reading starts
/// at the keyframe before the moment; an MPEG-TS has no index, so its seek lands ON the
/// moment and decodes late — seek to a while before (the lead) and read on, and a run that
/// ends in an error is swallowed (`try?`) because the frame it wrote is the right one. When
/// nothing was written, read from the start to the moment; still nothing is "no frame at X s".
pub fn frame_at(seconds: f64, url: &Path, pixels: usize, out: &Path) -> Result<(), ExtractError> {
    let at = if seconds > 0.0 { seconds } else { 0.0 }; // `max(0, seconds)`
    let rest: Vec<String> = [
        "-frames:v".to_string(),
        "1".into(),
        "-vf".into(),
        format!("scale='min(iw,{pixels})':'min(ih,{pixels})':force_original_aspect_ratio=decrease"),
        "-q:v".into(),
        "3".into(),
        "-y".into(),
        out.to_string_lossy().into_owned(),
    ]
    .to_vec();
    let lead = if rules::is_mpeg_transport_stream(&url.to_string_lossy()) {
        at.min(12.0) // x264 keeps up to 250 frames between keyframes
    } else {
        0.0
    };
    let mut arguments: Vec<String> = [
        "-v".to_string(),
        "error".into(),
        "-nostdin".into(),
        "-ss".into(),
        swift_interpolated(at - lead),
        "-i".into(),
        url.to_string_lossy().into_owned(),
    ]
    .to_vec();
    if lead > 0.0 {
        arguments.push("-ss".into());
        arguments.push(swift_interpolated(lead));
    }
    arguments.extend(rest.clone());
    let _ = ffmpeg::run(ffmpeg::ffmpeg().as_deref(), &arguments, &mut |_| Ok(())); // `try?`
    if !out.exists() {
        // Nothing written: read from the start to the moment.
        ffmpeg::run(
            ffmpeg::ffmpeg().as_deref(),
            &[
                "-v".into(),
                "error".into(),
                "-nostdin".into(),
                "-i".into(),
                url.to_string_lossy().into_owned(),
                "-ss".into(),
                swift_interpolated(at),
            ]
            .into_iter()
            .chain(rest)
            .collect::<Vec<String>>(),
            &mut |_| Ok(()),
        )
        .map_err(|error| error.extract())?;
    }
    if !out.exists() {
        return Err(ExtractError::Unreadable {
            why: format!("no frame at {} s", swift_interpolated(seconds)),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Windows;
    use std::path::Path;

    /// The engine's `windows` callback keeps ONE window in memory: every consumed hop
    /// leaves the buffer (`buffer.removeFirst(hopSamples)`, Media+Portable.swift:116), so a
    /// 3-hour podcast (345 M samples ≈ 690 MB of i16) streams past a 4–8 GB watchdog in
    /// constant memory. The port advanced a head index and never drained: the Vec grew
    /// with the whole file, doubled on the way, and the podcast spiked the box.
    ///
    /// The pin streams ONE HOUR of full-scale audio (16 kHz mono s16le = 115.4 M bytes,
    /// in 64 KiB reads exactly as ffmpeg's pipe delivers them) through the port's own
    /// feed callback and pins the process's resident size from /proc/self/statm — the
    /// watchdog's own instrument. The window/hop math is the engine's shipped pair
    /// (30 s windows, 15 s hops): 239 windows, each 30 s of full-scale samples over the
    /// silence gate (every one lands a WAV), and the tail (15 s new past the overlap,
    /// 15 s ≥ the 2 s minimum) emits the 240th.
    ///
    /// Measured on this box (2026-11-13, the drain installed / the head index installed,
    /// same fixture, same run): the drained stream adds < 32 MB of RSS; the head index
    /// adds the hour's buffer (~115 MB).
    #[test]
    fn a_long_stream_keeps_one_window_of_memory() {
        let dir = std::env::temp_dir().join(format!("digup-windows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the emit folder");

        let hour_samples: usize = 16_000 * 3_600;
        let chunk_samples: usize = 32_768; // 64 KiB of s16le, ffmpeg's read size
        let base = rss().expect("the process reports its resident size");
        let mut state = Windows {
            buffer: Vec::new(),
            offset: 0,
            window_samples: 30 * 16_000,
            hop_samples: 15 * 16_000,
            pieces: Vec::new(),
            odd: None,
        };
        let loud: Vec<u8> = {
            // Full-scale square: every window clears the −50 dBFS gate.
            let mut bytes = Vec::with_capacity(chunk_samples * 2);
            for _ in 0..chunk_samples {
                bytes.extend_from_slice(&20_000i16.to_le_bytes());
            }
            bytes
        };
        for _ in 0..hour_samples / chunk_samples {
            state.feed(&loud, &dir).expect("a window-sized feed");
        }
        // The hour does not divide the 64 KiB read: feed the part-chunk that completes it.
        let rest = (hour_samples - hour_samples / chunk_samples * chunk_samples) * 2;
        state
            .feed(&loud[..rest], &dir)
            .expect("the part-chunk that completes the hour");
        // The hour ends exactly at a window boundary (57.6 M = 120 windows with the
        // drains), so the tail is quiet; one hop of samples past it makes the shape the
        // engine sees on a real file: a window emits every hop, and the tail's new
        // audio past the overlap — window − hop = 15 s ≥ the 2 s minimum — emits too.
        // Window k emits when the buffer holds window + k*hop (the drains keep exactly
        // that): floor((total − window)/hop) + 1 = 239 windows across the hour, the tail
        // feeds the 240th (it completes a window at the file's end — the loop, not the
        // tail clause), and the tail clause sees an empty buffer after it. The offset
        // stands at the total: Swift's `offset` counts the hops, 240 × 240,000 = the hour.
        let tail: Vec<u8> = {
            let mut bytes = Vec::with_capacity(15 * 16_000 * 2);
            for _ in 0..15 * 16_000 {
                bytes.extend_from_slice(&20_000i16.to_le_bytes());
            }
            bytes
        };
        state.feed(&tail, &dir).expect("the tail's hop of samples");
        let peak = rss().expect("the resident size after the stream");

        assert_eq!(
            state.pieces.len(),
            240,
            "floor((total − window)/hop) = 240 drains: 239 whole windows across the hour \
             plus the tail's 240th (measured, drain shape)"
        );
        assert_eq!(
            state.offset, hour_samples as i64,
            "240 drains of a hop: the offset counts the hour"
        );

        let grew = peak.saturating_sub(base);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            grew < 32 * 1_048_576,
            "streaming an hour of audio added {grew} bytes of memory: the engine's callback \
             keeps ONE 30 s window (1 MB) because removeFirst discards each consumed hop \
             (Media+Portable.swift:116); the hour's samples standing beside the window is \
             the head-index shape (~115 MB), which on the 3-hour class of podcasts walks \
             into the watchdog's cap"
        );
    }

    fn rss() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4_096)
    }

    /// The drain leaves EXACTLY `window − hop` samples standing (the engine's overlap), and
    /// the tail's `buffer.count` is the same number — the emission math never shifts.
    #[test]
    fn the_drained_overlap_is_the_engine_s_window_minus_hop() {
        let dir = std::env::temp_dir().join(format!("digup-windows-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the emit folder");
        let mut state = Windows {
            buffer: Vec::new(),
            offset: 0,
            window_samples: 30 * 16_000,
            hop_samples: 15 * 16_000,
            pieces: Vec::new(),
            odd: None,
        };
        let mut feed = vec![0u8; (45 * 16_000) * 2]; // 45 s of samples, one read
        for i in (0..feed.len()).step_by(2) {
            feed[i..i + 2].copy_from_slice(&20_000i16.to_le_bytes());
        }
        state.feed(&feed, &dir).expect("45 s in one read");
        // 45 s of samples, 30 s window, 15 s hops: the FIRST window emits at 30 s and
        // the drain leaves EXACTLY window − hop standing... which for a 45 s stream is a
        // SECOND window: Swift's `while buffer.count >= windowSamples` runs the same
        // second round, and the second window (30..60 s) covers the audio 30..45 s.
        // The drain's contract this test pins: every window is followed by a hop-sized
        // removal, offset counts hops, and the standing tail is total − hop·n — the
        // numbers Swift's `removeFirst(hopSamples)` produces, digit for digit.
        assert_eq!(
            state.pieces.len(),
            2,
            "two windows across 45 s: 0..30 s and 30..45 s"
        );
        assert_eq!(
            state.buffer.len(),
            45 * 16_000 - 2 * 15 * 16_000,
            "the remainder stands: total − hop·n"
        );
        assert_eq!(state.offset, 2 * 15 * 16_000, "offset counts the hops");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = Path::new(&dir);
    }
}
