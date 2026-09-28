//! Where the archived files come from. Production reads the pipeline's S3 buckets (see the
//! `trackside-snapshot` binary); tests and offline runs read a local mirror or memory.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use async_trait::async_trait;

/// The two archive buckets Trackside reads, read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bucket {
    /// Racing Australia fields and form, and official results (`HorseRacing/JSON/<date>/`).
    Racing,
    /// The sectional canon tables (`canonical/sectional/<date>/`).
    Sectional,
}

impl Bucket {
    fn dir(self) -> &'static str {
        match self {
            Bucket::Racing => "racing",
            Bucket::Sectional => "sectional",
        }
    }
}

#[async_trait]
pub trait Archive: Send + Sync {
    /// Every key under `prefix`.
    async fn list(&self, bucket: Bucket, prefix: &str) -> Result<Vec<String>>;
    /// The object's bytes, or `None` when it does not exist.
    async fn get(&self, bucket: Bucket, key: &str) -> Result<Option<Vec<u8>>>;
}

/// A local mirror: `<root>/racing/<key>` and `<root>/sectional/<key>`.
pub struct LocalArchive {
    pub root: PathBuf,
}

#[async_trait]
impl Archive for LocalArchive {
    async fn list(&self, bucket: Bucket, prefix: &str) -> Result<Vec<String>> {
        let base = self.root.join(bucket.dir());
        let (dir, _) = prefix.rsplit_once('/').unwrap_or(("", prefix));
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(base.join(dir)) else {
            return Ok(out);
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let key = if dir.is_empty() {
                name
            } else {
                format!("{dir}/{name}")
            };
            if key.starts_with(prefix) {
                out.push(key);
            }
        }
        out.sort();
        Ok(out)
    }

    async fn get(&self, bucket: Bucket, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.root.join(bucket.dir()).join(key);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

/// An in-memory archive for tests.
#[derive(Default)]
pub struct MemoryArchive {
    pub objects: BTreeMap<(Bucket, String), Vec<u8>>,
}

impl MemoryArchive {
    pub fn put(&mut self, bucket: Bucket, key: &str, body: impl Into<Vec<u8>>) {
        self.objects.insert((bucket, key.to_string()), body.into());
    }
}

#[async_trait]
impl Archive for MemoryArchive {
    async fn list(&self, bucket: Bucket, prefix: &str) -> Result<Vec<String>> {
        Ok(self
            .objects
            .keys()
            .filter(|(b, k)| *b == bucket && k.starts_with(prefix))
            .map(|(_, k)| k.clone())
            .collect())
    }

    async fn get(&self, bucket: Bucket, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.objects.get(&(bucket, key.to_string())).cloned())
    }
}
