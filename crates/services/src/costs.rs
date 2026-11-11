//! What indexing costs on this machine (`MeasuredCosts.swift`, `Estimator.swift:7-21`): the M4
//! Pro's fixed `Costs`, this machine's tallies of seconds and units per kind, and the speed the
//! model check kept — all in `costs.json` beside the index.
//!
//! The JSON is a wire contract as much as the schema: the Swift engine writes it (pretty,
//! sorted keys) and the Swift app reads it, and `speed --model` runs on either side of the seam.
//! Field names, optionality and the tally layout below are the engine's.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// `Costs`: seconds of indexing per unit, measured 2026-10-07 on an M4 Pro with llama.cpp
/// (Q8_0), reading files while the model embeds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Costs {
    /// 140 tokens, decode included.
    pub image: f64,
    /// 280 tokens; its OCR (0.24 s) runs alongside and sets the pace.
    pub screenshot: f64,
    /// Text pages are cheap, rendered scans cost an image.
    pub pdf_page: f64,
    /// 40 at a time (a data dump's go to keyword search only, nearly free).
    pub doc_chunk: f64,
    /// A 30 s window every 25 s.
    pub audio_second: f64,
    /// A 70-token keyframe per shot (at most one per 3 s) + the soundtrack.
    pub video_second: f64,
    /// A stretch of code (2026-10-09: 258 Swift and Objective-C files, 1,245 stretches in 45 s).
    pub code_stretch: f64,
}

impl Default for Costs {
    fn default() -> Self {
        Self {
            image: 0.12,
            screenshot: 0.32,
            pdf_page: 0.09,
            doc_chunk: 0.045,
            audio_second: 0.0089,
            video_second: 0.019,
            code_stretch: 0.037,
        }
    }
}

impl Costs {
    fn set(&mut self, name: &str, value: f64) {
        match name {
            "image" => self.image = value,
            "screenshot" => self.screenshot = value,
            "pdfPage" => self.pdf_page = value,
            "docChunk" => self.doc_chunk = value,
            "audioSecond" => self.audio_second = value,
            "videoSecond" => self.video_second = value,
            "codeStretch" => self.code_stretch = value,
            _ => {}
        }
    }

    fn get(&self, name: &str) -> f64 {
        match name {
            "image" => self.image,
            "screenshot" => self.screenshot,
            "pdfPage" => self.pdf_page,
            "docChunk" => self.doc_chunk,
            "audioSecond" => self.audio_second,
            "videoSecond" => self.video_second,
            "codeStretch" => self.code_stretch,
            _ => 0.0,
        }
    }
}

/// `MeasuredCosts.Tally`: seconds spent for units of one kind, as indexing measured them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Tally {
    #[serde(default)]
    pub seconds: f64,
    #[serde(default)]
    pub units: f64,
}

/// `MeasuredCosts.Speed`: how this machine compares to the M4 Pro (1 is as fast, 2 takes twice
/// as long). `kinds` (from `speed --model`) stands in for `embedding`/`ocr` where present.
/// `device` — "gpu" or "cpu" — is the speed check's choice of where the model runs;
/// `checks` counts the timings; `doubtful` marks a check timed at a bad moment.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Speed {
    pub embedding: f64,
    pub ocr: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doubtful: Option<bool>,
}

/// `MeasuredCosts`: the tallies, the speed, and the costs to estimate with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MeasuredCosts {
    #[serde(default)]
    pub tallies: BTreeMap<String, Tally>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
}

/// `MeasuredCosts.enough`: units a kind needs before its measured cost is trusted — a few
/// dozen files, or minutes of media.
pub fn enough(name: &str) -> f64 {
    match name {
        "image" => 20.0,
        "screenshot" => 20.0,
        "pdfPage" => 30.0,
        "docChunk" => 30.0,
        "audioSecond" => 600.0,
        "videoSecond" => 600.0,
        "codeStretch" => 100.0,
        _ => 20.0,
    }
}

/// `MeasuredCosts.fields`, in the engine's order.
pub const KIND_FIELDS: [&str; 7] = [
    "image",
    "screenshot",
    "pdfPage",
    "docChunk",
    "audioSecond",
    "videoSecond",
    "codeStretch",
];

impl MeasuredCosts {
    /// `add`: a job that took `seconds` for `units`. Recent work counts most: past ten times
    /// what's enough, what came before is halved.
    pub fn add(&mut self, name: &str, units: f64, seconds: f64) {
        if units <= 0.0 || seconds <= 0.0 {
            return;
        }
        let tally = self.tallies.entry(name.to_string()).or_default();
        if tally.units > 10.0 * enough(name) {
            tally.seconds /= 2.0;
            tally.units /= 2.0;
        }
        tally.seconds += seconds;
        tally.units += units;
    }

    /// `pace`: what indexing here compares to what was expected (measured / expected, over the
    /// kinds with enough, weighted by their seconds). None before any kind has enough.
    pub fn pace(&self) -> Option<f64> {
        let expected = self.expected();
        let mut sum = 0.0;
        let mut weights = 0.0;
        for name in KIND_FIELDS {
            let Some(tally) = self.tallies.get(name) else {
                continue;
            };
            if tally.units < enough(name) || tally.seconds <= 0.0 {
                continue;
            }
            let expected_cost = expected.get(name);
            if expected_cost <= 0.0 {
                continue;
            }
            sum += tally.seconds * (tally.seconds / tally.units / expected_cost).ln();
            weights += tally.seconds;
        }
        if weights > 0.0 {
            Some((sum / weights).exp())
        } else {
            None
        }
    }

    /// `MeasuredCosts.expected`: the fixed costs as fast as `speed` says. `kinds` (measured with
    /// the model itself) per field; else the one embedding factor — a PC's processor can be fifty
    /// times slower than the M4 Pro's GPU, so both paths clamp.
    pub fn expected(&self) -> Costs {
        let mut costs = Costs::default();
        if let Some(kinds) = &self.speed.as_ref().and_then(|s| s.kinds.as_ref()) {
            for name in KIND_FIELDS {
                let factor = kinds.get(name).copied().unwrap_or(1.0).clamp(0.1, 500.0);
                costs.set(name, costs.get(name) * factor);
            }
        } else if let Some(speed) = &self.speed {
            let embedding = speed.embedding.clamp(0.25, 10.0);
            let ocr = speed.ocr.clamp(0.25, 10.0);
            for name in KIND_FIELDS {
                let factor = match name {
                    "screenshot" => embedding.max(ocr), // OCR runs beside the model and usually sets the pace
                    _ => embedding,
                };
                costs.set(name, costs.get(name) * factor);
            }
        }
        costs
    }

    /// `MeasuredCosts.costs`: the costs to estimate with — measured ones where a kind has
    /// enough of them, the fixed ones (scaled by speed) times the measured pace elsewhere.
    pub fn costs(&self) -> Costs {
        let mut costs = self.expected();
        let pace = self.pace().unwrap_or(1.0).clamp(0.02, 50.0);
        for name in KIND_FIELDS {
            match self.tallies.get(name) {
                Some(tally) if tally.units >= enough(name) => {
                    costs.set(name, tally.seconds / tally.units);
                }
                _ => {
                    let value = costs.get(name);
                    costs.set(name, value * pace);
                }
            }
        }
        costs
    }

    /// `MeasuredCosts.file`.
    pub fn file(folder: &Path) -> std::path::PathBuf {
        folder.join("costs.json")
    }

    /// `MeasuredCosts.load`: what's saved in `folder`, or nothing measured yet.
    pub fn load(folder: &Path) -> Self {
        std::fs::read(Self::file(folder))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// `MeasuredCosts.save`: pretty and key-sorted, as the Swift `JSONEncoder` writes it.
    fn save(&self, folder: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(folder)?;
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        let text =
            serde_json::to_string_pretty(&sort_json(value)).map_err(std::io::Error::other)?;
        std::fs::write(Self::file(folder), text)
    }

    /// `MeasuredCosts.setSpeed`: saves this machine's speed beside what's measured, and forgets
    /// what indexing measured if asked (the model moved from the GPU to the processor, or back).
    pub fn set_speed(speed: Speed, folder: &Path, forget_tallies: bool) {
        let mut saved = Self::load(folder);
        saved.speed = Some(speed);
        if forget_tallies {
            saved.tallies.clear();
        }
        let _ = saved.save(folder);
    }

    /// `MeasuredCosts.merge`: adds `new` to what's saved in `folder` (the main index and the
    /// code index each add their own kinds).
    pub fn merge(&self, folder: &Path) {
        let mut saved = Self::load(folder);
        for (name, tally) in &self.tallies {
            saved.add(name, tally.units, tally.seconds);
        }
        let _ = saved.save(folder);
    }
}

/// Key-sorted output (the Swift encoder's `.sortedKeys`): serde_json's `to_string_pretty`
/// keeps a `BTreeMap` order for maps we built with BTreeMaps, and object key order of
/// structs comes from field order — sorting the whole value keeps it stable for the
/// diff the cross-check does.
fn sort_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, sort_json(item)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect::<serde_json::Map<_, _>>(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_json).collect())
        }
        other => other,
    }
}
