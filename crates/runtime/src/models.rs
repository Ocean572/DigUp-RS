//! The EmbeddingGemma 2 files the engine runs (`ModelFiles.swift` + `LlamaModels.swift`): the
//! ggml-org Q8_0 conversion at one pinned revision, pinned by size and sha256 — never the F16
//! files, because the model's activations overflow float16.
//!
//! The pins are the contract: the app's download, the CI download and the corpus manifests all
//! hash-match these, so a Rust port that checks the same hashes loads the same weights.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// One pinned file: its name, its exact size, its sha256 in lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub name: &'static str,
    pub bytes: i64,
    pub sha256: &'static str,
}

/// `ModelFiles`: the two files, the repo and the revision they are read from.
pub struct ModelFiles;

impl ModelFiles {
    pub const REPOSITORY: &'static str = "ggml-org/embeddinggemma-2-GGUF";
    pub const REVISION: &'static str = "bfcd298762cc34d0357ece5ebdd31791a3a374d8";

    /// The text model (310 MB): all that queries need.
    pub const TEXT: ModelFile = ModelFile {
        name: "embeddinggemma-2-Q8_0.gguf",
        bytes: 309_855_456,
        sha256: "2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135",
    };
    /// The image and audio encoders (555 MB).
    pub const MEDIA: ModelFile = ModelFile {
        name: "mmproj-embeddinggemma-2-Q8_0.gguf",
        bytes: 554_821_024,
        sha256: "c4a8a52691ecef40618438928bdf9e68379b854e24166f292592353db0aab64f",
    };
    /// In download order.
    pub const ALL: [ModelFile; 2] = [Self::TEXT, Self::MEDIA];

    /// Hugging Face's address for this file at the pinned revision — the `resolve`
    /// route, as `ModelFiles.swift:23` spells it
    /// (`https://huggingface.co/<repo>/resolve/<rev>/<name>`). Measured on this box
    /// 2026-11-13: `/<repo>/<rev>/<file>` answers **404** (52,885 bytes of text/html —
    /// the web UI's "not found" page), `/<repo>/resolve/<rev>/<file>` answers **200**,
    /// `application/octet-stream`, content-length 309,855,456 — the file. Without
    /// `resolve`, `download-model` downloads an HTML page into the models folder,
    /// the sha256 gate rejects it, and the helper dies on the model it never fetched.
    pub fn url(file: &ModelFile) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            Self::REPOSITORY,
            Self::REVISION,
            file.name
        )
    }

    /// Where the app keeps them (`ModelFiles.installed` = `AppPaths.applicationSupport/Models`).
    pub fn installed() -> PathBuf {
        application_support().join("Models")
    }

    /// The pinned revision's folder in the Hugging Face cache (`hf download` puts files there):
    /// `$HF_HUB_CACHE`, `$HF_HOME/hub`, or `~/.cache/huggingface/hub`.
    pub fn hugging_face_snapshot() -> PathBuf {
        let hub = env_dir("HF_HUB_CACHE")
            .or_else(|| env_dir("HF_HOME").map(|home| home.join("hub")))
            .unwrap_or_else(|| home().join(".cache/huggingface/hub"));
        let repo = Self::REPOSITORY.replace('/', "--");
        hub.join(format!("models--{repo}"))
            .join("snapshots")
            .join(Self::REVISION)
    }

    /// `file` in the Hugging Face cache when it is there at the right size (its checksum is for
    /// the caller to check, exactly as the engine leaves it).
    pub fn cached_copy(file: &ModelFile) -> Option<PathBuf> {
        let path = Self::hugging_face_snapshot()
            .join(file.name)
            .canonicalize()
            .ok()?;
        let size = std::fs::metadata(&path).ok()?.len() as i64;
        (size == file.bytes).then_some(path)
    }

    /// The files that aren't in `folder` yet. Only verified files get their final name, so being
    /// there is enough.
    pub fn missing(folder: &Path) -> Vec<&'static ModelFile> {
        Self::ALL
            .iter()
            .filter(|file| !folder.join(file.name).exists())
            .collect()
    }

    /// The SHA-256 of a file as lowercase hex, read in 8 MB pieces (a 555 MB file takes about
    /// half a second).
    pub fn sha256(path: &Path) -> crate::Result<String> {
        let file = std::fs::File::open(path).map_err(|e| crate::Error::io(path, e))?;
        let mut reader = std::io::BufReader::with_capacity(8 << 20, file);
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 8 << 20];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|e| crate::Error::io(path, e))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    /// `ModelFiles.totalBytes`.
    pub fn total_bytes() -> i64 {
        Self::ALL.iter().map(|file| file.bytes).sum()
    }
}

/// One user-supplied face weight (FACES.md Phase 0 #1): the pin carries its own
/// URL because the packs live in three different Hugging Face repositories, and
/// the **sha256 is the pin** — `resolve/main/` is a moving branch, so a fetch
/// that arrives with a different hash is refused (the download loop's
/// "arrived damaged, three times"), never accepted under a renamed name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceModelFile {
    pub name: &'static str,
    pub bytes: i64,
    pub sha256: &'static str,
    pub url: &'static str,
}

/// `FaceModelFiles`: the user-supplied tier packs (FACES.md §3 Model Tiers).
/// **None of these ships in a payload tile** — they enter a machine only
/// through the consent gate's user-initiated `download-model --faces`, and the
/// hashes below are the ones measured on this box 2026-11-21 (`/tmp/opencode/
/// face-models/SHA256SUMS`) against the same URLs the reference harness used.
pub struct FaceModelFiles;

impl FaceModelFiles {
    /// The SCRFD-10G detector (WIDER FACE lineage, non-commercial research
    /// terms) — the parity tier with Immich's detection.
    pub const DET: FaceModelFile = FaceModelFile {
        name: "scrfd_10g_bnkps.onnx",
        bytes: 16_923_827,
        sha256: "5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91",
        url: "https://huggingface.co/fal/AuraFace-v1/resolve/main/scrfd_10g_bnkps.onnx",
    };
    /// The balanced embedder (buffalo_s recognition, ArcFace w600k).
    pub const W600K_MBF: FaceModelFile = FaceModelFile {
        name: "w600k_mbf.onnx",
        bytes: 13_616_099,
        sha256: "9cc6e4a75f0e2bf0b1aed94578f144d15175f357bdc05e815e5c4a02b319eb4f",
        url: "https://huggingface.co/immich-app/buffalo_s/resolve/main/recognition/model.onnx",
    };
    /// The parity embedder (buffalo_l recognition — Immich's default).
    pub const W600K_R50: FaceModelFile = FaceModelFile {
        name: "w600k_r50.onnx",
        bytes: 174_383_860,
        sha256: "4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43",
        url: "https://huggingface.co/immich-app/buffalo_l/resolve/main/recognition/model.onnx",
    };
    /// AdaFace IR-18 (MIT repo weights, WebFace4M lineage).
    pub const ADAFACE_IR18: FaceModelFile = FaceModelFile {
        name: "adaface_ir_18.onnx",
        bytes: 96_105_345,
        sha256: "6b6a35772fb636cdd4fa86520c1a259d0c41472a76f70f802b351837a00d9870",
        url: "https://huggingface.co/yakhyo/uniface-weights/resolve/main/adaface_ir_18.onnx",
    };
    /// AdaFace IR-50 (MIT repo weights, WebFace600K lineage).
    pub const ADAFACE_IR50: FaceModelFile = FaceModelFile {
        name: "adaface_ir_50.onnx",
        bytes: 174_396_180,
        sha256: "9c0ae385d7362323c92d218de598d32e52a9130b78faac6a8a924091aca62c0b",
        url: "https://huggingface.co/yakhyo/uniface-weights/resolve/main/adaface_ir_50.onnx",
    };
    /// AdaFace IR-101 — the accuracy tier (FACES.md §3: beats Immich's
    /// `w600k_r50` on cross-pose/low-quality; WebFace12M lineage).
    pub const ADAFACE_IR101: FaceModelFile = FaceModelFile {
        name: "adaface_ir_101.onnx",
        bytes: 260_704_652,
        sha256: "f2eb07d03de0af560a82e1214df799fec5e09375d43521e2868f9dc387e5a43e",
        url: "https://huggingface.co/yakhyo/uniface-weights/resolve/main/adaface_ir_101.onnx",
    };
    /// In download order: the detector first (a scan needs it before any embed).
    pub const ALL: [FaceModelFile; 6] = [
        Self::DET,
        Self::W600K_MBF,
        Self::W600K_R50,
        Self::ADAFACE_IR18,
        Self::ADAFACE_IR50,
        Self::ADAFACE_IR101,
    ];

    /// The files that aren't in `folder` yet (the same law as `ModelFiles`:
    /// only a checksum-verified file ever gets its final name).
    pub fn missing(folder: &Path) -> Vec<&'static FaceModelFile> {
        Self::ALL
            .iter()
            .filter(|file| !folder.join(file.name).exists())
            .collect()
    }

    /// `FaceModelFiles.totalBytes`.
    pub fn total_bytes() -> i64 {
        Self::ALL.iter().map(|file| file.bytes).sum()
    }
}

/// Where the EmbeddingGemma 2 files for llama.cpp are (`LlamaModels`, LlamaModels.swift).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaModels {
    pub text: PathBuf,
    /// None while only the text part is there.
    pub media: Option<PathBuf>,
}

/// `DIGUP_MODELS`: the environment name that pins the model folder — the ONE home for the
/// string. The server publishes it on every helper spawn (`EngineConfig::env` in
/// `server/src/config.rs`), the shell sets it for its helpers, and `folders()` below is the
/// only reader.
pub const MODELS_ENV: &str = "DIGUP_MODELS";

impl LlamaModels {
    pub const REPOSITORY: &'static str = ModelFiles::REPOSITORY;
    pub const REVISION: &'static str = ModelFiles::REVISION;
    pub const TEXT_FILE: &'static str = ModelFiles::TEXT.name;
    pub const MEDIA_FILE: &'static str = ModelFiles::MEDIA.name;

    /// The folders to look in, first match wins. `$DIGUP_MODELS` is the app's choice and the only
    /// place it looks then: the developer's Hugging Face cache mustn't hide a missing download.
    /// Without it (the CLI): Application Support, then the Hugging Face cache.
    pub fn folders() -> Vec<PathBuf> {
        if let Some(chosen) = env_dir(MODELS_ENV) {
            return vec![chosen];
        }
        vec![ModelFiles::installed(), ModelFiles::hugging_face_snapshot()]
    }

    /// The first folder with the text part. Missing files are an error, so an indexing run stops
    /// and its files stay pending.
    pub fn locate() -> crate::Result<Self> {
        let folders = Self::folders();
        for folder in &folders {
            let text = folder.join(Self::TEXT_FILE);
            if !text.is_file() {
                continue;
            }
            let media = folder.join(Self::MEDIA_FILE);
            return Ok(Self {
                text,
                media: media.is_file().then_some(media),
            });
        }
        Err(crate::Error::NotFound(format!(
            "no {} in {}; get it with: hf download {} {} {} --revision {}",
            Self::TEXT_FILE,
            folders
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            Self::REPOSITORY,
            Self::TEXT_FILE,
            Self::MEDIA_FILE,
            Self::REVISION,
        )))
    }

    /// Both files are there, so indexing can run (checked without loading anything).
    pub fn complete() -> bool {
        Self::locate()
            .ok()
            .and_then(|models| models.media)
            .is_some()
    }
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home() -> PathBuf {
    // Slashes on Windows (identity on unix): folder paths built on this feed fs calls
    // (which accept slashes) and JSON event fields compared engine-side.
    PathBuf::from(digup_core::paths::to_slashes(
        &env_dir("HOME")
            .unwrap_or_else(|| PathBuf::from("."))
            .to_string_lossy(),
    ))
}

/// `AppPaths.applicationSupport` (Portable.swift:44-63): macOS `~/Library/Application Support/DigUp`,
/// Windows `%LOCALAPPDATA%\DigUp`, Linux `$XDG_DATA_HOME/digup` or `~/.local/share/digup`.
fn application_support() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/DigUp")
    }
    #[cfg(windows)]
    {
        env_dir("LOCALAPPDATA")
            .map(|dir| dir.join("DigUp"))
            .unwrap_or_else(|| home().join("AppData/Local/DigUp"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env_dir("XDG_DATA_HOME")
            .map(|dir| dir.join("digup"))
            .unwrap_or_else(|| home().join(".local/share/digup"))
    }
}

#[cfg(test)]
mod tests {
    use super::ModelFiles;

    /// The download URL is a wire contract: `ModelFiles.swift:23` builds
    /// `https://huggingface.co/<repo>/resolve/<rev>/<name>`, and the SHA is a
    /// hex-only block (`[0-9a-f]{40}`, ModelFiles.swift) — the one spelling the
    /// Hugging Face CDN serves. Measured on this box 2026-11-13:
    ///
    ///   /<repo>/<rev>/embeddinggemma-2-Q8_0.gguf ......... 404, 52,885 B text/html
    ///   /<repo>/resolve/<rev>/embeddinggemma-2-Q8_0.gguf  200, application/
    ///                                                        octet-stream, 309,855,456 B
    ///
    /// Without `resolve` the verb downloads the web UI's not-found page into the
    /// models folder, the sha256 gate rejects it, and `download-model` dies on a
    /// model it never fetched. Pinned at the source, byte for byte, for both files.
    #[test]
    fn the_download_url_is_the_resolve_route() {
        for file in &ModelFiles::ALL {
            let url = ModelFiles::url(file);
            let expected = format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                ModelFiles::REPOSITORY,
                ModelFiles::REVISION,
                file.name
            );
            assert_eq!(url, expected, "the download URL is Swift's URL: {url}");
            assert!(
                url.contains("/resolve/"),
                "the CDN serves files only on the resolve route: {url}"
            );
            // The revision is the pinned commit hash: hex, 40 characters, no ref name.
            let rev = ModelFiles::REVISION;
            assert_eq!(rev.len(), 40);
            assert!(rev.chars().all(|c| c.is_ascii_hexdigit()));
        }
        // Both models, exact — the strings the `download-model` verb puts on curl's argv
        // (crates/cli/src/main.rs: `ModelFiles::url(file)`).
        assert_eq!(
            ModelFiles::url(&ModelFiles::TEXT),
            "https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/\
bfcd298762cc34d0357ece5ebdd31791a3a374d8/embeddinggemma-2-Q8_0.gguf"
        );
        assert_eq!(
            ModelFiles::url(&ModelFiles::MEDIA),
            "https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/\
bfcd298762cc34d0357ece5ebdd31791a3a374d8/mmproj-embeddinggemma-2-Q8_0.gguf"
        );
    }
}
