use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchRecord {
    pub fetch_id: String,
    pub requested_url: String,
    pub final_url: String,
    pub mode: String,
    pub requested_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub status: String,
    pub bytes: u64,
    pub content_hash: String,
    pub content_markdown: String,
    pub source_provider: String,
    pub policy_decision: String,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub entry_id: String,
    pub fetch_id: Option<String>,
    pub tool: String,
    pub args_redacted: serde_json::Value,
    pub outcome: String,
    pub error: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchBundleRecord {
    pub assignment_id: String,
    pub request_hash: String,
    pub response_json: String,
}

#[derive(Clone)]
pub struct Store {
    backend: Arc<StoreBackend>,
}

enum StoreBackend {
    Disk { root: PathBuf },
    NoOp,
}

impl Store {
    pub async fn disk(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        tokio::fs::create_dir_all(root.join("fetches"))
            .await
            .with_context(|| format!("creating {}/fetches", root.display()))?;
        tokio::fs::create_dir_all(root.join("audits"))
            .await
            .with_context(|| format!("creating {}/audits", root.display()))?;
        tokio::fs::create_dir_all(root.join("bundles"))
            .await
            .with_context(|| format!("creating {}/bundles", root.display()))?;
        Ok(Self {
            backend: Arc::new(StoreBackend::Disk { root }),
        })
    }

    /// No-op store for focused unit and protocol tests.
    pub fn noop() -> Self {
        Self {
            backend: Arc::new(StoreBackend::NoOp),
        }
    }

    pub async fn write_fetch(&self, rec: &FetchRecord) -> Result<()> {
        let StoreBackend::Disk { root } = self.backend.as_ref() else {
            return Ok(());
        };
        write_json(
            root.join("fetches").join(format!("{}.json", rec.fetch_id)),
            rec,
        )
        .await
    }

    pub async fn read_fetch(&self, fetch_id: &str) -> Result<Option<FetchRecord>> {
        let StoreBackend::Disk { root } = self.backend.as_ref() else {
            return Ok(None);
        };
        if !valid_id(fetch_id) {
            anyhow::bail!("invalid fetch id");
        }
        let path = root.join("fetches").join(format!("{fetch_id}.json"));
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding {}", path.display()))
            .map(Some)
    }

    pub async fn write_audit(&self, rec: &AuditRecord) -> Result<()> {
        let StoreBackend::Disk { root } = self.backend.as_ref() else {
            return Ok(());
        };
        write_json(
            root.join("audits").join(format!("{}.json", rec.entry_id)),
            rec,
        )
        .await
    }

    pub async fn write_research_bundle(&self, rec: &ResearchBundleRecord) -> Result<()> {
        let StoreBackend::Disk { root } = self.backend.as_ref() else {
            return Ok(());
        };
        write_json(
            root.join("bundles").join(bundle_key(&rec.assignment_id)),
            rec,
        )
        .await
    }

    pub async fn read_research_bundle(
        &self,
        assignment_id: &str,
    ) -> Result<Option<ResearchBundleRecord>> {
        let StoreBackend::Disk { root } = self.backend.as_ref() else {
            return Ok(None);
        };
        let path = root.join("bundles").join(bundle_key(assignment_id));
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        let record: ResearchBundleRecord = serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding {}", path.display()))?;
        if record.assignment_id != assignment_id {
            anyhow::bail!("research bundle key collision");
        }
        Ok(Some(record))
    }

    pub fn new_fetch_id() -> String {
        Uuid::new_v4().to_string()
    }

    pub fn new_audit_id() -> String {
        Uuid::new_v4().to_string()
    }

    pub fn content_hash(body: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(body);
        format!("sha256:{:x}", hasher.finalize())
    }
}

fn bundle_key(assignment_id: &str) -> String {
    format!(
        "{}.json",
        Store::content_hash(assignment_id.as_bytes()).replace(':', "-")
    )
}

async fn write_json(path: PathBuf, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("serializing evidence record")?;
    let tmp = path.with_extension(format!("json.{}.tmp", Uuid::new_v4()));
    tokio::fs::write(&tmp, bytes)
        .await
        .with_context(|| format!("writing {}", tmp.display()))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .with_context(|| format!("committing {}", path.display()))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disk_store_round_trips_fetches() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let store = Store::disk(dir.path()).await.expect("disk store");
        let now = Utc::now();
        let record = FetchRecord {
            fetch_id: Store::new_fetch_id(),
            requested_url: "https://example.com".into(),
            final_url: "https://example.com/".into(),
            mode: "scrape".into(),
            requested_at: now,
            completed_at: now,
            status: "ok".into(),
            bytes: 5,
            content_hash: Store::content_hash(b"hello"),
            content_markdown: "hello".into(),
            source_provider: "fixture".into(),
            policy_decision: "allow".into(),
            truncated: false,
        };
        store.write_fetch(&record).await.expect("write fetch");
        let loaded = store
            .read_fetch(&record.fetch_id)
            .await
            .expect("read fetch")
            .expect("fetch exists");
        assert_eq!(loaded.content_hash, record.content_hash);
        assert_eq!(loaded.content_markdown, "hello");
    }

    #[tokio::test]
    async fn disk_store_round_trips_colon_scoped_research_bundles() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let store = Store::disk(dir.path()).await.expect("disk store");
        let record = ResearchBundleRecord {
            assignment_id: "run-id:primary-evidence".into(),
            request_hash: "sha256:request".into(),
            response_json: r#"{"content":{"source_count":4}}"#.into(),
        };
        store
            .write_research_bundle(&record)
            .await
            .expect("write bundle");
        let loaded = store
            .read_research_bundle(&record.assignment_id)
            .await
            .expect("read bundle")
            .expect("bundle exists");
        assert_eq!(loaded.assignment_id, record.assignment_id);
        assert_eq!(loaded.request_hash, record.request_hash);
        assert_eq!(loaded.response_json, record.response_json);
    }

    #[test]
    fn rejects_path_like_ids() {
        assert!(!valid_id("../secret"));
        assert!(valid_id("0c90152d-b74f-44de-9acf-435550c3d1c6"));
    }
}
