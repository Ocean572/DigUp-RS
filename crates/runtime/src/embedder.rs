//! EmbeddingGemma 2 on llama.cpp, in this process (`LlamaEmbedder.swift`): the image and audio
//! encoders (mtmd) feed the text model, and its mean-pooled, L2-normalized output is the vector.
//!
//! Inputs are framed as the reference frames them: `<bos> text <eos>`, `<bos> <marker> <eos>`
//! for a picture cut to its token budget (`ImageSizing`) and for sound cut at 30 s. The model is
//! non-causal with no KV cache use, so each input — token ids and encoder rows together — goes
//! through one `llama_process` call, as llama-server's mixed batches do. The exactness is what
//! `ReferenceVectorTests` measures (cosine ≥ 0.999 against stored vectors): the prompt, the cut
//! rule, the resizing, one sequence per input, the position array, the pooling and the
//! normalization all show up in the numbers.

use std::cell::Cell;
use std::ffi::CString;
use std::path::Path;
use std::time::Instant;

use crate::backend::{self, threads};
use crate::ffi;
use crate::worker::{Embedder, Embedding, WorkerInfo};
use crate::{Error, LlamaModels, Result};

/// `LlamaEmbedder.vectorVersion`: what this pipeline's vectors are (the model files, llama.cpp,
/// the resizing, the prompts). Indexes record this, not the llama.cpp build, so an update that
/// leaves the vectors alone keeps them (b11461 made version 1).
pub const VECTOR_VERSION: i32 = 1;

/// `LlamaEmbedder.budgets`: the vision soft-token budgets an image may be embedded at.
pub const BUDGETS: [i32; 5] = [70, 140, 280, 560, 1120];

/// `LlamaEmbedder.maxSequences`: how many inputs one batch carries.
const MAX_SEQUENCES: i32 = 16;

/// `LlamaEmbedder.modelInfo`: what every LlamaEmbedder reports (its `vectorSource` goes into index
/// fingerprints), known without loading it.
pub fn model_info() -> WorkerInfo {
    WorkerInfo {
        model: "google/embeddinggemma-2".to_string(),
        revision: LlamaModels::REVISION.to_string(),
        runtime: format!("llama.cpp-vectors{VECTOR_VERSION}"),
        dtype: "text-q8_0+mmproj-q8_0".to_string(),
        dim: digup_core::vectors::DIMENSION,
        text_only: false,
        load_seconds: None,
    }
}

pub struct LlamaEmbedder {
    model: *mut ffi::llama_model,
    context: *mut ffi::llama_context,
    media: *mut ffi::mtmd_context,
    batch: *mut ffi::llama_batch_ext,
    vocab: *const ffi::llama_vocab,
    /// 768: the output projection.
    embedding_size: usize,
    /// The width of encoder rows fed to the text model (`llama_model_n_embd_inp`): mtmd's image
    /// and audio rows are handed to the batch at this width.
    row_size: usize,
    max_tokens: usize,
    info: WorkerInfo,
    // The batch's state, as the engine's `sequences` / `tokens` / `position`.
    sequences: Cell<i32>,
    tokens: Cell<usize>,
    position: Cell<ffi::llama_pos>,
}

impl LlamaEmbedder {
    /// `text_only`: skip the media encoders (queries, and the text-only indexing start).
    /// `context_tokens`: the context/budget width — defaults like before (`text_only ? 512 : 2048`):
    /// 512 is a *query* budget; indexing at 512 would silently change its vectors.
    /// `log`: where llama.cpp's warnings and errors go (stderr when None).
    pub fn new(
        models: &LlamaModels,
        text_only: bool,
        context_tokens: Option<usize>,
        log: Option<&Path>,
    ) -> Result<Self> {
        let started = Instant::now();
        backend::init();
        backend::set_log(log);
        if !text_only && models.media.is_none() {
            return Err(Error::NotFound(format!(
                "{} (images and audio) isn't downloaded yet",
                LlamaModels::MEDIA_FILE
            )));
        }

        let mut model_params = unsafe { ffi::llama_model_default_params() };
        model_params.n_gpu_layers = if backend::cpu_only() { 0 } else { 999 };
        let text_path = cstring(&models.text);
        let model = unsafe { ffi::llama_model_load_from_file(text_path.as_ptr(), model_params) };
        if model.is_null() {
            return Err(Error::Llama(format!(
                "can't load {}",
                models.text.display()
            )));
        }

        // One input must fit one micro-batch: 2048 tokens (the reference's cut), which also holds a
        // 1120-token image or a 750-token 30 s clip. Queries are short, so the query encoder
        // reserves less and cuts at 512.
        let max_tokens = context_tokens.unwrap_or(if text_only { 512 } else { 2048 });
        let mut params = unsafe { ffi::llama_context_default_params() };
        params.n_ctx = max_tokens as u32;
        params.n_batch = max_tokens as u32;
        params.n_ubatch = max_tokens as u32;
        params.n_seq_max = MAX_SEQUENCES as u32;
        params.kv_unified = true; // each input may use the whole context, as in llama-embedding
        params.embeddings = true;
        params.pooling_type = ffi::llama_pooling_type_LLAMA_POOLING_TYPE_MEAN;
        if !cfg!(target_os = "macos") {
            params.n_threads = threads();
            params.n_threads_batch = threads();
        }
        let context = unsafe { ffi::llama_init_from_model(model, params) };
        if context.is_null() {
            unsafe { ffi::llama_model_free(model) };
            return Err(Error::Llama("can't create a llama.cpp context".to_string()));
        }

        let mut media: *mut ffi::mtmd_context = std::ptr::null_mut();
        if let (Some(media_file), false) = (&models.media, text_only) {
            let mut media_params = unsafe { ffi::mtmd_context_params_default() };
            media_params.use_gpu = !backend::cpu_only();
            media_params.print_timings = false;
            media_params.warmup = true;
            media_params.image_min_tokens = 1; // never rescale: images arrive at their final size (ImageSizing)
            media_params.image_max_tokens = 1120;
            if !cfg!(target_os = "macos") {
                media_params.n_threads = threads();
            }
            let media_path = cstring(media_file);
            media = unsafe { ffi::mtmd_init_from_file(media_path.as_ptr(), model, media_params) };
            if media.is_null() {
                unsafe {
                    ffi::llama_free(context);
                    ffi::llama_model_free(model);
                }
                return Err(Error::Llama(format!("can't load {}", media_file.display())));
            }
        }

        let batch = unsafe { ffi::llama_batch_ext_init(context) };
        if batch.is_null() {
            unsafe {
                ffi::llama_free(context);
                ffi::llama_model_free(model);
            }
            return Err(Error::Llama("can't create a llama.cpp batch".to_string()));
        }

        let known = model_info();
        let mut embedder = Self {
            model,
            context,
            media,
            batch,
            vocab: unsafe { ffi::llama_model_get_vocab(model) },
            embedding_size: unsafe { ffi::llama_model_n_embd_out(model) } as usize,
            row_size: unsafe { ffi::llama_model_n_embd_inp(model) } as usize,
            max_tokens,
            info: WorkerInfo {
                dim: unsafe { ffi::llama_model_n_embd_out(model) } as usize,
                text_only,
                ..known
            },
            sequences: Cell::new(0),
            tokens: Cell::new(0),
            position: Cell::new(0),
        };

        // The first calls build the GPU's pipelines: pay that now (the panel is opening).
        if let Err(e) = embedder.warm_up() {
            embedder.close();
            return Err(e);
        }

        let seconds = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
        embedder.info.load_seconds = Some(seconds);
        backend::line(&format!(
            "ready in {seconds} s ({}, llama.cpp {}, vectors {VECTOR_VERSION})",
            if text_only { "text-only" } else { "full" },
            crate::build_tag(),
        ));
        Ok(embedder)
    }

    fn warm_up(&self) -> Result<()> {
        let warm = [crate::prompts::query("warm up")];
        self.embed_texts(&warm)?;
        self.embed_texts(&warm)?;
        Ok(())
    }

    /// One file's vector, or why it has none (an unreadable file must not sink its neighbours).
    /// The reason goes into the file's `detail`, capped at 300 characters as the engine caps it.
    fn one(made: Result<Vec<f32>>) -> Embedding {
        match made {
            Ok(vector) => Embedding::made(vector),
            Err(e) => {
                let text = e.to_string();
                Embedding::failed(text.chars().take(300).collect::<String>())
            }
        }
    }

    /// `<bos> text <eos>`, cut to `max_tokens` the way transformers cuts (keeping both special tokens).
    fn tokenize(&self, text: &str) -> Vec<ffi::llama_token> {
        let text_c = cstring_str(text);
        let mut tokens = vec![0 as ffi::llama_token; text_c.as_bytes().len() + 8];
        let mut count = self.tokenize_into(&text_c, &mut tokens);
        if count < 0 {
            // The buffer was too small: llama.cpp reports how many it needed.
            tokens = vec![0 as ffi::llama_token; (-count) as usize];
            count = self.tokenize_into(&text_c, &mut tokens);
        }
        tokens.truncate(count.max(0) as usize);
        if tokens.len() > self.max_tokens {
            let eos = unsafe { ffi::llama_vocab_eos(self.vocab) };
            tokens.truncate(self.max_tokens - 1);
            tokens.push(eos);
        }
        tokens
    }

    fn tokenize_into(&self, text: &CString, tokens: &mut [ffi::llama_token]) -> i32 {
        unsafe {
            ffi::llama_tokenize(
                self.vocab,
                text.as_ptr(),
                text.as_bytes().len() as i32,
                tokens.as_mut_ptr(),
                tokens.len() as i32,
                true,
                true,
            )
        }
    }

    /// Starts the next input in the batch (each input is its own sequence).
    fn begin(&self) {
        self.sequences.set(self.sequences.get() + 1);
        self.position.set(0);
    }

    /// Drops everything added so far, including an input that failed halfway.
    fn reset(&self) {
        unsafe { ffi::llama_batch_ext_clear(self.batch) };
        self.sequences.set(0);
        self.tokens.set(0);
    }

    fn add_token(&self, token: ffi::llama_token) -> Result<()> {
        let index =
            unsafe { ffi::llama_batch_ext_add_token(self.batch, self.sequences.get() - 1, token) };
        self.place(index)
    }

    fn place(&self, index: i32) -> Result<()> {
        if index < 0 {
            return Err(Error::Llama(format!("batch full ({index})")));
        }
        let positions = [self.position.get(), 0, 0, 0];
        let placed = unsafe { ffi::llama_batch_ext_set_pos(self.batch, index, positions.as_ptr()) };
        let outputs = unsafe { ffi::llama_batch_ext_set_output_embd(self.batch, index, true) };
        if !placed || !outputs {
            return Err(Error::Llama(format!("can't place token {index}")));
        }
        self.position.set(self.position.get() + 1);
        self.tokens.set(self.tokens.get() + 1);
        Ok(())
    }

    /// Runs the text model over the batch and returns one L2-normalized vector per input, in order.
    fn run(&self) -> Result<Vec<Vec<f32>>> {
        let sequences = self.sequences.get();
        let status = unsafe {
            ffi::llama_process(
                self.context,
                ffi::llama_process_type_LLAMA_PROCESS_TYPE_ENCODE,
                self.batch,
            )
        };
        if status != 0 {
            self.reset();
            return Err(Error::Llama(format!("llama_process failed ({status})")));
        }
        let vectors = (0..sequences)
            .map(|sequence| self.pooled(sequence))
            .collect::<Result<Vec<_>>>();
        self.reset(); // the batch is cleared on the way out, as the engine's `defer` does
        vectors
    }

    /// Mean-pooled output → L2-normalized. The length is summed in double (as the engine's
    /// `run()` does) so the scale is the model's, not a rounded one.
    fn pooled(&self, sequence: i32) -> Result<Vec<f32>> {
        let pooled = unsafe { ffi::llama_get_embeddings_seq(self.context, sequence) };
        if pooled.is_null() {
            return Err(Error::Llama(format!(
                "no pooled embedding for input {sequence}"
            )));
        }
        let mut sum = 0.0f64;
        for index in 0..self.embedding_size {
            let value = unsafe { *pooled.add(index) } as f64;
            sum += value * value;
        }
        let scale = if sum > 0.0 {
            (1.0 / sum.sqrt()) as f32
        } else {
            0.0
        };
        Ok((0..self.embedding_size)
            .map(|index| unsafe { *pooled.add(index) } * scale)
            .collect())
    }

    // MARK: Media

    /// One file's vector: mtmd's chunks of tokens and encoder rows, in one batch, in one sequence
    /// (`embedMedia`). The vector is the text model's mean-pooled output over the whole input.
    fn embed_media(&self, path: &str, image: bool, budget: i32) -> Result<Vec<f32>> {
        let bitmap = self.load_media(path, image, budget)?;
        let chunks = unsafe { ffi::mtmd_input_chunks_init() };
        let marker = CString::new(
            unsafe { std::ffi::CStr::from_ptr(ffi::mtmd_default_marker()) }
                .to_string_lossy()
                .into_owned(),
        )
        .expect("the media marker is plain text");
        let bitmaps = [bitmap];
        let text = ffi::mtmd_input_text {
            text: marker.as_ptr(),
            text_len: marker.as_bytes().len(),
            add_special: true,
            parse_special: true,
        };
        let status =
            unsafe { ffi::mtmd_tokenize(self.media, chunks, &text, bitmaps.as_ptr().cast(), 1) };
        let result = (|| {
            if status != 0 {
                return Err(Error::Llama(format!("can't tokenize {path} ({status})")));
            }
            self.reset();
            self.begin();
            let count = unsafe { ffi::mtmd_input_chunks_size(chunks) };
            for index in 0..count {
                let chunk = unsafe { ffi::mtmd_input_chunks_get(chunks, index) };
                if unsafe { ffi::mtmd_input_chunk_get_type(chunk) }
                    == ffi::mtmd_input_chunk_type_MTMD_INPUT_CHUNK_TYPE_TEXT
                {
                    let mut tokens = 0usize;
                    let start =
                        unsafe { ffi::mtmd_input_chunk_get_tokens_text(chunk, &mut tokens) };
                    if start.is_null() {
                        continue;
                    }
                    for i in 0..tokens {
                        self.add_token(unsafe { *start.add(i) })?;
                    }
                } else {
                    let encoded = unsafe { ffi::mtmd_encode_chunk(self.media, chunk) };
                    let rows = unsafe { ffi::mtmd_get_output_embd(self.media) };
                    if encoded != 0 || rows.is_null() {
                        return Err(Error::Llama(format!("the encoder failed on {path}")));
                    }
                    self.add_rows(rows, unsafe { ffi::mtmd_input_chunk_get_n_tokens(chunk) })?;
                }
            }
            Ok(self.run()?[0].clone())
        })();
        unsafe {
            ffi::mtmd_bitmap_free(bitmap);
            ffi::mtmd_input_chunks_free(chunks);
        }
        result
    }

    /// Reads an image or sound with mtmd's helpers (stb_image, miniaudio at 16 kHz), resizes
    /// images for `budget` (`ImageSizing`: mtmd sees the pixels the reference saw) and cuts sound
    /// at 30 s, as the reference does. The caller frees the bitmap.
    fn load_media(&self, path: &str, image: bool, budget: i32) -> Result<*mut ffi::mtmd_bitmap> {
        let c_path = CString::new(path).map_err(|_| Error::Llama(format!("can't open {path}")))?;
        let wrapper = unsafe {
            ffi::mtmd_helper_bitmap_init_from_file(
                self.media,
                c_path.as_ptr(),
                false,
                ffi::mtmd_helper_init_opt_default(),
            )
        };
        if !wrapper.video_ctx.is_null() {
            // A video's frames and sound come through as their own files, as the engine's video
            // helper splits them; the file itself is not one input.
            unsafe {
                if !wrapper.bitmap.is_null() {
                    ffi::mtmd_bitmap_free(wrapper.bitmap);
                }
                ffi::mtmd_helper_video_free(wrapper.video_ctx);
            }
            return Err(Error::Llama(format!(
                "video goes in as frames and sound, not as a file: {path}"
            )));
        }
        let bitmap = wrapper.bitmap;
        if bitmap.is_null() {
            return Err(Error::Llama(format!("can't read {path}")));
        }
        if (unsafe { ffi::mtmd_bitmap_is_audio(bitmap) }) == image {
            unsafe { ffi::mtmd_bitmap_free(bitmap) };
            return Err(Error::Llama(format!(
                "not {}: {path}",
                if image { "an image" } else { "audio" }
            )));
        }
        if image {
            let width = unsafe { ffi::mtmd_bitmap_get_nx(bitmap) } as usize;
            let height = unsafe { ffi::mtmd_bitmap_get_ny(bitmap) } as usize;
            let target = match crate::image_sizing::target_size(height, width, budget) {
                Some(target) => target,
                None => {
                    unsafe { ffi::mtmd_bitmap_free(bitmap) };
                    return Err(Error::Llama(format!("image too thin to embed: {path}")));
                }
            };
            if target == (height, width) {
                return Ok(bitmap);
            }
            let pixels = {
                let data = unsafe { ffi::mtmd_bitmap_get_data(bitmap) };
                let length = width * height * 3;
                crate::image_sizing::resize_bicubic(
                    unsafe { std::slice::from_raw_parts(data, length) },
                    width,
                    height,
                    target.1,
                    target.0,
                )
            };
            let resized =
                unsafe { ffi::mtmd_bitmap_init(target.1 as u32, target.0 as u32, pixels.as_ptr()) };
            unsafe { ffi::mtmd_bitmap_free(bitmap) };
            if resized.is_null() {
                return Err(Error::Llama(format!("can't resize {path}")));
            }
            return Ok(resized);
        }
        let samples = unsafe { ffi::mtmd_bitmap_get_n_bytes(bitmap) } / std::mem::size_of::<f32>();
        let limit = 30 * unsafe { ffi::mtmd_get_audio_sample_rate(self.media) } as usize;
        if samples <= limit {
            return Ok(bitmap);
        }
        // Past 30 s: keep the first 30, as the reference's audio window does.
        let prefix = unsafe {
            ffi::mtmd_bitmap_init_from_audio(limit, ffi::mtmd_bitmap_get_data(bitmap) as *const f32)
        };
        unsafe { ffi::mtmd_bitmap_free(bitmap) };
        if prefix.is_null() {
            return Err(Error::Llama(format!("can't cut {path} at 30 s")));
        }
        Ok(prefix)
    }

    /// Encoder rows, one per batch entry: the batch copies each row, so the pointer only has to
    /// stay alive for the call (`add(rows:count:)`). mtmd returns the rows packed `row_size`
    /// floats apart, so each row starts `row * row_size` in.
    fn add_rows(&self, rows: *mut f32, count: usize) -> Result<()> {
        for row in 0..count {
            let embd = ffi::llama_embd {
                data: unsafe { rows.add(row * self.row_size) } as *const f32,
                n_rows: 1,
                n_embd: self.row_size,
            };
            let index = unsafe {
                ffi::llama_batch_ext_add_embd(self.batch, self.sequences.get() - 1, embd)
            };
            self.place(index)?;
        }
        Ok(())
    }
}

impl Embedder for LlamaEmbedder {
    fn info(&self) -> &WorkerInfo {
        &self.info
    }

    /// One batch per call, packed to `max_tokens` with at most `MAX_SEQUENCES` inputs, as the
    /// engine's `embedTexts` packs them.
    fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.reset();
        let mut vectors = Vec::new();
        for text in texts {
            let tokens = self.tokenize(text);
            if self.sequences.get() == MAX_SEQUENCES
                || self.tokens.get() + tokens.len() > self.max_tokens
            {
                vectors.extend(self.run()?);
            }
            self.begin();
            for token in tokens {
                self.add_token(token)?;
            }
        }
        if self.sequences.get() > 0 {
            vectors.extend(self.run()?);
        }
        Ok(vectors)
    }

    fn embed_images(&self, paths: &[String], budget: i32) -> Result<Vec<Embedding>> {
        if self.media.is_null() {
            return Err(Error::Worker(
                "this embedder was started text-only".to_string(),
            ));
        }
        if !BUDGETS.contains(&budget) {
            return Err(Error::Worker(format!("budget must be one of {BUDGETS:?}")));
        }
        Ok(paths
            .iter()
            .map(|path| Self::one(self.embed_media(path, true, budget)))
            .collect())
    }

    fn embed_audio(&self, paths: &[String]) -> Result<Vec<Embedding>> {
        if self.media.is_null() {
            return Err(Error::Worker(
                "this embedder was started text-only".to_string(),
            ));
        }
        Ok(paths
            .iter()
            .map(|path| Self::one(self.embed_media(path, false, 0)))
            .collect())
    }

    fn close(&mut self) {
        unsafe {
            if !self.batch.is_null() {
                ffi::llama_batch_ext_free(self.batch);
                self.batch = std::ptr::null_mut();
            }
            if !self.media.is_null() {
                ffi::mtmd_free(self.media);
                self.media = std::ptr::null_mut();
            }
            if !self.context.is_null() {
                ffi::llama_free(self.context);
                self.context = std::ptr::null_mut();
            }
            if !self.model.is_null() {
                ffi::llama_model_free(self.model);
                self.model = std::ptr::null_mut();
            }
        }
    }
}

impl Drop for LlamaEmbedder {
    fn drop(&mut self) {
        self.close();
    }
}

fn cstring(path: &Path) -> CString {
    CString::new(path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| CString::new("").expect("empty path"))
}

/// A C string of text whose NULs would cut it short: llama.cpp tokenizes a C string, and the
/// engine hands it the text's UTF-8 bytes with one terminating NUL.
fn cstring_str(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_else(|_| CString::new("").expect("empty text"))
}
