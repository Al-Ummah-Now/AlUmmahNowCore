//! The Whisper model catalogue and on-disk model management.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Tiny,
    Base,
    Small,
    Medium,
    LargeV3Turbo,
}

/// One downloadable model. See `docs/MODELS.md` for provenance and licences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub tier: ModelTier,
    /// `q5_1`, `q8_0`, … for quantised variants.
    pub quantization: Option<String>,
    pub file_name: String,
    pub url: String,
    pub size_bytes: u64,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
}

fn entry(
    id: &str,
    tier: ModelTier,
    quantization: Option<&str>,
    file_name: &str,
    size_bytes: u64,
    sha256: &str,
) -> ModelDescriptor {
    ModelDescriptor {
        id: id.to_owned(),
        tier,
        quantization: quantization.map(str::to_owned),
        file_name: file_name.to_owned(),
        url: format!("{BASE_URL}/{file_name}"),
        size_bytes,
        sha256: sha256.to_owned(),
    }
}

/// Every supported model: multilingual only, since Arabic is a first-class language.
pub fn model_catalog() -> Vec<ModelDescriptor> {
    vec![
        entry(
            "tiny-q5_1",
            ModelTier::Tiny,
            Some("q5_1"),
            "ggml-tiny-q5_1.bin",
            32_152_673,
            "818710568da3ca15689e31a743197b520007872ff9576237bda97bd1b469c3d7",
        ),
        entry(
            "tiny-q8_0",
            ModelTier::Tiny,
            Some("q8_0"),
            "ggml-tiny-q8_0.bin",
            43_537_433,
            "c2085835d3f50733e2ff6e4b41ae8a2b8d8110461e18821b09a15c40c42d1cca",
        ),
        entry(
            "tiny",
            ModelTier::Tiny,
            None,
            "ggml-tiny.bin",
            77_691_713,
            "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        ),
        entry(
            "base-q5_1",
            ModelTier::Base,
            Some("q5_1"),
            "ggml-base-q5_1.bin",
            59_707_625,
            "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
        ),
        entry(
            "base-q8_0",
            ModelTier::Base,
            Some("q8_0"),
            "ggml-base-q8_0.bin",
            81_768_585,
            "c577b9a86e7e048a0b7eada054f4dd79a56bbfa911fbdacf900ac5b567cbb7d9",
        ),
        entry(
            "base",
            ModelTier::Base,
            None,
            "ggml-base.bin",
            147_951_465,
            "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        ),
        entry(
            "small-q5_1",
            ModelTier::Small,
            Some("q5_1"),
            "ggml-small-q5_1.bin",
            190_085_487,
            "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        ),
        entry(
            "small-q8_0",
            ModelTier::Small,
            Some("q8_0"),
            "ggml-small-q8_0.bin",
            264_464_607,
            "49c8fb02b65e6049d5fa6c04f81f53b867b5ec9540406812c643f177317f779f",
        ),
        entry(
            "small",
            ModelTier::Small,
            None,
            "ggml-small.bin",
            487_601_967,
            "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        ),
        entry(
            "medium-q5_0",
            ModelTier::Medium,
            Some("q5_0"),
            "ggml-medium-q5_0.bin",
            539_212_467,
            "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
        ),
        entry(
            "medium-q8_0",
            ModelTier::Medium,
            Some("q8_0"),
            "ggml-medium-q8_0.bin",
            823_369_779,
            "42a1ffcbe4167d224232443396968db4d02d4e8e87e213d3ee2e03095dea6502",
        ),
        entry(
            "medium",
            ModelTier::Medium,
            None,
            "ggml-medium.bin",
            1_533_763_059,
            "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
        ),
        entry(
            "large-v3-turbo-q5_0",
            ModelTier::LargeV3Turbo,
            Some("q5_0"),
            "ggml-large-v3-turbo-q5_0.bin",
            574_041_195,
            "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        ),
        entry(
            "large-v3-turbo-q8_0",
            ModelTier::LargeV3Turbo,
            Some("q8_0"),
            "ggml-large-v3-turbo-q8_0.bin",
            874_188_075,
            "317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1",
        ),
        entry(
            "large-v3-turbo",
            ModelTier::LargeV3Turbo,
            None,
            "ggml-large-v3-turbo.bin",
            1_624_555_275,
            "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        ),
    ]
}

/// Whether and how a model is present locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStatus {
    pub descriptor: ModelDescriptor,
    pub installed: bool,
    /// Bytes of an interrupted download that can be resumed.
    pub partial_bytes: u64,
}

/// What a fetch learned from the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchInfo {
    /// Whether the body starts at the requested offset (HTTP 206) rather than at zero.
    pub resumed: bool,
    pub total_bytes: Option<u64>,
}

/// Where a [`ModelFetcher`] delivers bytes.
pub trait ChunkSink: Send {
    /// Called once before any data.
    fn begin(&mut self, info: FetchInfo) -> Result<()>;
    fn write(&mut self, chunk: &[u8]) -> Result<()>;
}

/// Downloads a URL. Injected so tests never reach the network and so native apps can use their own
/// HTTP stack.
#[async_trait]
pub trait ModelFetcher: Send + Sync {
    /// Requests `url` from byte `offset` (a range request when non-zero). Must honour `cancel`.
    async fn fetch(
        &self,
        url: &str,
        offset: u64,
        sink: &mut dyn ChunkSink,
        cancel: &CancellationToken,
    ) -> Result<()>;
}

/// Receives download progress from a worker: keep it cheap.
pub trait DownloadListener: Send + Sync {
    fn on_progress(&self, downloaded_bytes: u64, total_bytes: u64);
}

/// Installs, verifies and removes models under one directory.
pub struct ModelManager {
    dir: PathBuf,
    catalog: Vec<ModelDescriptor>,
    fetcher: Arc<dyn ModelFetcher>,
    active: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for ModelManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelManager")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// Removes the id from the active set when a download ends, however it ends.
struct ActiveGuard<'a> {
    active: &'a Mutex<HashSet<String>>,
    id: String,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

struct FileSink<'a> {
    path: &'a Path,
    file: Option<File>,
    written: u64,
    expected: u64,
    listener: &'a dyn DownloadListener,
}

impl ChunkSink for FileSink<'_> {
    fn begin(&mut self, info: FetchInfo) -> Result<()> {
        let mut options = OpenOptions::new();
        options.create(true).write(true);
        if info.resumed {
            options.append(true);
        } else {
            options.truncate(true);
            self.written = 0;
        }
        self.file = Some(options.open(self.path)?);
        Ok(())
    }

    fn write(&mut self, chunk: &[u8]) -> Result<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| Error::Internal("fetcher wrote before begin".to_owned()))?;
        self.written += chunk.len() as u64;
        if self.written > self.expected {
            return Err(Error::Network(
                "download is larger than the catalogue says".to_owned(),
            ));
        }
        file.write_all(chunk)?;
        self.listener.on_progress(self.written, self.expected);
        Ok(())
    }
}

fn file_len(path: &Path) -> Result<u64> {
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        match file.read(&mut buffer)? {
            0 => break,
            n => hasher.update(&buffer[..n]),
        }
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

impl ModelManager {
    /// Manages models in `dir` using the built-in catalogue.
    pub fn new(dir: impl Into<PathBuf>, fetcher: Arc<dyn ModelFetcher>) -> Self {
        Self::with_catalog(dir, model_catalog(), fetcher)
    }

    /// As [`Self::new`] with a custom catalogue, which is how tests point at a mock server.
    pub fn with_catalog(
        dir: impl Into<PathBuf>,
        catalog: Vec<ModelDescriptor>,
        fetcher: Arc<dyn ModelFetcher>,
    ) -> Self {
        Self {
            dir: dir.into(),
            catalog,
            fetcher,
            active: Mutex::new(HashSet::new()),
        }
    }

    fn descriptor(&self, id: &str) -> Result<&ModelDescriptor> {
        self.catalog
            .iter()
            .find(|d| d.id == id)
            .ok_or_else(|| Error::NotFound(format!("model {id}")))
    }

    fn final_path(&self, d: &ModelDescriptor) -> PathBuf {
        self.dir.join(&d.file_name)
    }

    fn part_path(&self, d: &ModelDescriptor) -> PathBuf {
        self.dir.join(format!("{}.part", d.file_name))
    }

    fn is_installed(&self, d: &ModelDescriptor) -> Result<bool> {
        Ok(file_len(&self.final_path(d))? == d.size_bytes)
    }

    /// Every catalogue model and its local state.
    pub fn list(&self) -> Result<Vec<ModelStatus>> {
        self.catalog
            .iter()
            .map(|d| {
                Ok(ModelStatus {
                    descriptor: d.clone(),
                    installed: self.is_installed(d)?,
                    partial_bytes: file_len(&self.part_path(d))?,
                })
            })
            .collect()
    }

    /// The installed model's file, if there is one.
    pub fn model_path(&self, id: &str) -> Result<Option<String>> {
        let d = self.descriptor(id)?;
        Ok(self
            .is_installed(d)?
            .then(|| self.final_path(d).to_string_lossy().into_owned()))
    }

    /// Bytes used by complete and partial models in the directory.
    pub fn disk_usage_bytes(&self) -> Result<u64> {
        let mut total = 0;
        for d in &self.catalog {
            total += file_len(&self.final_path(d))? + file_len(&self.part_path(d))?;
        }
        Ok(total)
    }

    /// Removes a model and any partial download.
    pub fn delete(&self, id: &str) -> Result<()> {
        let d = self.descriptor(id)?;
        if self.is_active(id) {
            return Err(Error::Conflict(format!("model {id} is downloading")));
        }
        remove_if_present(&self.final_path(d))?;
        remove_if_present(&self.part_path(d))
    }

    /// Re-hashes an installed model; `false` means the file has been damaged. Blocking.
    pub fn verify(&self, id: &str) -> Result<bool> {
        let d = self.descriptor(id)?;
        if !self.is_installed(d)? {
            return Ok(false);
        }
        Ok(sha256_file(&self.final_path(d))? == d.sha256)
    }

    fn is_active(&self, id: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(id)
    }

    /// Downloads a model, resuming an interrupted attempt, and verifies its checksum before
    /// installing it. Returns the model's path. A partial file is kept across failures and
    /// cancellation so the next attempt can resume.
    pub async fn download(
        &self,
        id: &str,
        listener: Arc<dyn DownloadListener>,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let d = self.descriptor(id)?.clone();
        let final_path = self.final_path(&d);
        if self.is_installed(&d)? {
            return Ok(final_path.to_string_lossy().into_owned());
        }
        if !self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned())
        {
            return Err(Error::Conflict(format!("model {id} is already downloading")));
        }
        let _guard = ActiveGuard {
            active: &self.active,
            id: id.to_owned(),
        };

        fs::create_dir_all(&self.dir)?;
        let part = self.part_path(&d);
        let mut offset = file_len(&part)?;
        if offset > d.size_bytes {
            remove_if_present(&part)?;
            offset = 0;
        }
        if offset < d.size_bytes {
            let mut sink = FileSink {
                path: &part,
                file: None,
                written: offset,
                expected: d.size_bytes,
                listener: listener.as_ref(),
            };
            self.fetcher.fetch(&d.url, offset, &mut sink, cancel).await?;
            if let Some(file) = sink.file.take() {
                file.sync_all()?;
            }
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if file_len(&part)? != d.size_bytes {
            return Err(Error::Network(format!(
                "download of {id} ended early; it can be resumed"
            )));
        }

        let hash_path = part.clone();
        let digest = tokio::task::spawn_blocking(move || sha256_file(&hash_path))
            .await
            .map_err(|e| Error::Internal(e.to_string()))??;
        if digest != d.sha256 {
            remove_if_present(&part)?;
            return Err(Error::Network(format!(
                "checksum mismatch for {id}; the download was discarded"
            )));
        }
        fs::rename(&part, &final_path)?;
        Ok(final_path.to_string_lossy().into_owned())
    }
}

#[cfg(feature = "http-download")]
pub use http::HttpModelFetcher;

#[cfg(feature = "http-download")]
mod http {
    use super::{ChunkSink, FetchInfo, ModelFetcher};
    use async_trait::async_trait;
    use aun_common::{Error, Result};
    use tokio_util::sync::CancellationToken;

    fn network(e: reqwest::Error) -> Error {
        Error::Network(e.without_url().to_string())
    }

    /// A [`ModelFetcher`] over `reqwest`.
    #[derive(Debug, Clone, Default)]
    pub struct HttpModelFetcher {
        client: reqwest::Client,
    }

    impl HttpModelFetcher {
        pub fn new() -> Self {
            Self::default()
        }
    }

    #[async_trait]
    impl ModelFetcher for HttpModelFetcher {
        async fn fetch(
            &self,
            url: &str,
            offset: u64,
            sink: &mut dyn ChunkSink,
            cancel: &CancellationToken,
        ) -> Result<()> {
            let mut request = self.client.get(url);
            if offset > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            }
            let mut response = tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Cancelled),
                r = request.send() => r.map_err(network)?,
            };
            let status = response.status();
            if !status.is_success() {
                return Err(Error::from_status(
                    status.as_u16(),
                    None,
                    format!("GET model: {status}"),
                ));
            }
            let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
            let length = response.content_length();
            sink.begin(FetchInfo {
                resumed,
                total_bytes: length.map(|l| if resumed { l + offset } else { l }),
            })?;
            loop {
                let chunk = tokio::select! {
                    _ = cancel.cancelled() => return Err(Error::Cancelled),
                    c = response.chunk() => c.map_err(network)?,
                };
                match chunk {
                    Some(bytes) => sink.write(&bytes)?,
                    None => return Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_well_formed() {
        let catalog = model_catalog();
        assert_eq!(catalog.len(), 15);
        let mut ids = HashSet::new();
        for d in &catalog {
            assert!(ids.insert(d.id.clone()), "duplicate {}", d.id);
            assert_eq!(d.sha256.len(), 64, "{}", d.id);
            assert!(d.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{}", d.id);
            assert!(d.url.ends_with(&d.file_name));
            assert!(d.size_bytes > 1_000_000);
            assert!(!d.file_name.contains(".en"), "multilingual only: {}", d.id);
        }
        for tier in [
            ModelTier::Tiny,
            ModelTier::Base,
            ModelTier::Small,
            ModelTier::Medium,
            ModelTier::LargeV3Turbo,
        ] {
            let tier_entries: Vec<_> = catalog.iter().filter(|d| d.tier == tier).collect();
            assert_eq!(tier_entries.len(), 3, "{tier:?}");
            assert_eq!(
                tier_entries.iter().filter(|d| d.quantization.is_none()).count(),
                1
            );
        }
    }
}
