//! Model-download cache using `huggingface_hub`'s on-disk layout, so Python tools can share it.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// A download its progress callback stopped; the `.incomplete` file stays resumable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "download stopped by caller")
    }
}

impl std::error::Error for Stopped {}

/// Whether `err` is a caller-requested stop rather than a transfer failure.
pub fn is_stopped(err: &anyhow::Error) -> bool {
    err.downcast_ref::<Stopped>().is_some()
}

/// Env vars consulted (in order) for an HF access token.
const HF_TOKEN_ENV_VARS: &[&str] = &["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "HUGGINGFACE_TOKEN"];

/// Root of the HF-compatible cache.
pub struct HfCache {
    root: PathBuf,
    token: Option<String>,
}

/// A handle to one HF repo within the cache.
pub struct HfRepo<'a> {
    cache: &'a HfCache,
    repo_id: String,
    revision: String,
}

/// A handle to one file within a repo.
pub struct HfFetch<'a> {
    repo: &'a HfRepo<'a>,
    filename: String,
}

impl HfCache {
    /// Construct a cache rooted at `$HF_HOME` if set, else `{data_dir}/hf_cache`.
    pub fn new(data_dir: &Path) -> Self {
        let root = match std::env::var_os("HF_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => {
                let mut p = data_dir.to_path_buf();
                p.push("hf_cache");
                p
            }
        };
        let token = discover_token(&root);
        Self { root, token }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `{root}/hub` — parent of all `models--*` repo folders.
    pub fn hub_dir(&self) -> PathBuf {
        let mut p = self.root.clone();
        p.push("hub");
        p
    }

    /// `{root}/token` — Python-compatible token file location.
    pub fn token_path(&self) -> PathBuf {
        let mut p = self.root.clone();
        p.push("token");
        p
    }

    /// Cached HF access token, if any was discovered at construction.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Build a repo handle (default revision = `"main"`).
    pub fn repo(&self, repo_id: impl Into<String>) -> HfRepo<'_> {
        HfRepo {
            cache: self,
            repo_id: repo_id.into(),
            revision: "main".to_string(),
        }
    }
}

/// Resolve the token via env vars first, then the `{root}/token` file.
fn discover_token(root: &Path) -> Option<String> {
    for var in HF_TOKEN_ENV_VARS {
        if let Ok(v) = std::env::var(var) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    let mut token_file = root.to_path_buf();
    token_file.push("token");
    match std::fs::read_to_string(&token_file) {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(_) => None,
    }
}

impl<'a> HfRepo<'a> {
    /// Override the revision (branch / tag / commit) — default is `"main"`.
    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = revision.into();
        self
    }

    /// `models--{org}--{repo}` (slashes in `repo_id` become `--`).
    pub fn folder_name(&self) -> String {
        format!("models--{}", self.repo_id).replace('/', "--")
    }

    /// `{hub_dir}/models--{org}--{repo}`.
    pub fn folder_path(&self) -> PathBuf {
        let mut p = self.cache.hub_dir();
        p.push(self.folder_name());
        p
    }

    /// `{folder_path}/refs/{branch}` — text file holding a commit hash.
    pub fn refs_path(&self, branch: &str) -> PathBuf {
        let mut p = self.folder_path();
        p.push("refs");
        p.push(branch);
        p
    }

    /// `{folder_path}/snapshots/{commit}` — directory of symlinks to blobs.
    pub fn snapshot_dir(&self, commit: &str) -> PathBuf {
        let mut p = self.folder_path();
        p.push("snapshots");
        p.push(commit);
        p
    }

    /// `{folder_path}/blobs/{etag}` — content-addressed real file.
    pub fn blob_path(&self, etag: &str) -> PathBuf {
        let mut p = self.folder_path();
        p.push("blobs");
        p.push(etag);
        p
    }

    pub fn file(&self, filename: impl Into<String>) -> HfFetch<'_> {
        HfFetch {
            repo: self,
            filename: filename.into(),
        }
    }
}

impl<'a> HfFetch<'a> {
    /// `{snapshot_dir}/{filename}` — where the symlink to the blob will live.
    pub fn pointer_path(&self, commit: &str) -> PathBuf {
        let mut p = self.repo.snapshot_dir(commit);
        p.push(&self.filename);
        p
    }

    pub fn blob_path(&self, etag: &str) -> PathBuf {
        self.repo.blob_path(etag)
    }

    /// `https://huggingface.co/{repo_id}/resolve/{revision}/{filename}`.
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo.repo_id,
            url_escape_revision(&self.repo.revision),
            self.filename
        )
    }
}

/// Percent-encode `/` in a revision so branches like `feature/x` survive the URL.
fn url_escape_revision(rev: &str) -> String {
    rev.replace('/', "%2F")
}

// ── URL parsing ──────────────────────────────────────────────────────────────

/// Split an HF `resolve` URL into `(repo_id, revision, filename)`; `None` if it isn't one.
pub fn parse_hf_url(url: &str) -> Option<(String, String, String)> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, rest) = after_scheme.split_once('/')?;
    if host != "huggingface.co" {
        return None;
    }
    // Need at least: {org}/{repo}/resolve/{rev}/{file...}
    let parts: Vec<&str> = rest.splitn(5, '/').collect();
    if parts.len() < 5 {
        return None;
    }
    if parts[2] != "resolve" {
        return None;
    }
    let org = parts[0];
    let repo = parts[1];
    let revision = parts[3];
    let filename = parts[4];
    if org.is_empty() || repo.is_empty() || revision.is_empty() || filename.is_empty() {
        return None;
    }
    Some((
        format!("{org}/{repo}"),
        urldecode_simple(revision),
        filename.to_string(),
    ))
}

/// Decode `%2F` → `/` (just enough for revisions). Leaves other escapes alone.
fn urldecode_simple(s: &str) -> String {
    s.replace("%2F", "/").replace("%2f", "/")
}

// ── Host policy ──────────────────────────────────────────────────────────────

/// Hosts (HF and its CloudFront CDN domains) that may get the bearer token across a redirect.
pub(crate) fn should_send_auth_on_redirect(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "huggingface.co"
        || host.ends_with(".huggingface.co")
        || host.ends_with(".cloudfront.net")
}

// ── Redirect-aware reqwest client ────────────────────────────────────────────

/// Client with redirects off: we follow them by hand to send the token only to HF/CDN hosts.
pub fn build_redirect_aware_client(_token: Option<&str>) -> Result<reqwest::Client> {
    use reqwest::redirect::Policy;

    reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|e| anyhow!("failed to build redirect-aware client: {e}"))
}

/// Strip a leading and trailing `"` (HTTP etag quoting).
fn unquote_etag(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

impl<'a> HfFetch<'a> {
    /// Resumable, etag-aware download to the content-addressed blob path. `progress` returning
    /// `false` returns [`Stopped`], keeping `{blob}.incomplete` to resume; delete it to cancel.
    pub async fn download_to_blob<F>(
        &self,
        client: &reqwest::Client,
        token: Option<&str>,
        mut progress: F,
    ) -> Result<PathBuf>
    where
        F: FnMut(u64, u64) -> bool,
    {
        use tokio::io::AsyncWriteExt as _;

        let url = self.url();

        // Manual redirect follow so we can strip auth on cross-host hops.
        let head_resp = head_with_redirects(client, &url, token).await?;
        let headers = head_resp.headers();

        let etag = headers
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(unquote_etag)
            .or_else(|| {
                headers
                    .get("x-linked-etag")
                    .and_then(|v| v.to_str().ok())
                    .map(unquote_etag)
            })
            .ok_or_else(|| anyhow!("HEAD {url}: no etag / x-linked-etag header"))?;
        if etag.is_empty() {
            return Err(anyhow!("HEAD {url}: empty etag"));
        }

        let total: u64 = headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let commit = headers
            .get("x-repo-commit")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "main".to_string());

        let blob_path = self.blob_path(&etag);
        let blobs_dir = blob_path
            .parent()
            .ok_or_else(|| anyhow!("blob path has no parent: {}", blob_path.display()))?;
        tokio::fs::create_dir_all(blobs_dir)
            .await
            .with_context(|| format!("create blobs dir {}", blobs_dir.display()))?;

        // ── Fast path: blob already complete on disk ────────────────────────
        if let Ok(meta) = tokio::fs::metadata(&blob_path).await {
            if total == 0 || meta.len() == total {
                progress(meta.len(), meta.len());
                finalize_pointers(self, &etag, &commit, &blob_path).await?;
                return Ok(blob_path);
            }
        }

        // ── Resumable download into {blob}.incomplete ───────────────────────
        let final_url = head_resp.final_url.clone();
        let mut incomplete_path = blob_path.clone();
        let incomplete_name = format!(
            "{}.incomplete",
            blob_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&etag)
        );
        incomplete_path.set_file_name(incomplete_name);

        // ── Per-blob advisory lock — at most one process does the GET ───────
        // Released when `_lock_guard` drops or the process exits.
        let lock_path = {
            let mut p = blob_path.clone();
            p.set_file_name(format!("{etag}.lock"));
            p
        };
        let lock_outcome = acquire_blob_lock(
            &lock_path,
            &blob_path,
            &incomplete_path,
            total,
            &mut progress,
        )
        .await?;
        let _lock_guard = match lock_outcome {
            LockOutcome::Acquired(guard) => guard,
            LockOutcome::AnotherFinished => {
                finalize_pointers(self, &etag, &commit, &blob_path).await?;
                return Ok(blob_path);
            }
        };

        // The previous lock holder may have just finished the blob; re-check the fast path.
        if let Ok(meta) = tokio::fs::metadata(&blob_path).await {
            if total == 0 || meta.len() == total {
                progress(meta.len(), meta.len());
                finalize_pointers(self, &etag, &commit, &blob_path).await?;
                return Ok(blob_path);
            }
        }

        let existing_size = match tokio::fs::metadata(&incomplete_path).await {
            Ok(m) => m.len(),
            Err(_) => 0,
        };

        let range_header: Option<String> = if existing_size > 0 {
            Some(format!("bytes={existing_size}-"))
        } else {
            None
        };
        let resp = get_with_redirects(client, &final_url, token, range_header.as_deref()).await?;

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(existing_size > 0)
            .write(true)
            .truncate(existing_size == 0)
            .open(&incomplete_path)
            .await
            .with_context(|| format!("open {}", incomplete_path.display()))?;

        let mut downloaded: u64 = existing_size;
        if !progress(downloaded, total) {
            file.flush().await.ok();
            return Err(anyhow!(Stopped));
        }

        let mut resp = resp;
        while let Some(chunk) = resp
            .chunk()
            .await
            .with_context(|| format!("read chunk from {final_url}"))?
        {
            file.write_all(&chunk)
                .await
                .with_context(|| format!("write {}", incomplete_path.display()))?;
            downloaded += chunk.len() as u64;
            if !progress(downloaded, total) {
                // Keep `.incomplete`: the next call resumes from it, so a stop is a pause.
                file.flush().await.ok();
                return Err(anyhow!(Stopped));
            }
        }
        file.flush().await.ok();
        drop(file);

        tokio::fs::rename(&incomplete_path, &blob_path)
            .await
            .with_context(|| {
                format!(
                    "rename {} -> {}",
                    incomplete_path.display(),
                    blob_path.display()
                )
            })?;

        finalize_pointers(self, &etag, &commit, &blob_path).await?;
        Ok(blob_path)
    }
}

// ── Per-blob advisory locking ────────────────────────────────────────────────

/// Polls while another process downloads the same blob; at 1 s each, a 10-minute cap.
const BLOB_LOCK_MAX_POLLS: u32 = 600;
const BLOB_LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Advisory lock held for one download attempt; dropping the `File` releases it.
struct BlobLockGuard {
    _file: std::fs::File,
}

enum LockOutcome {
    Acquired(BlobLockGuard),
    /// While waiting, the blob appeared on disk — another process finished.
    AnotherFinished,
}

/// Take an exclusive lock on `lock_path`, or poll until the blob appears or the holder dies,
/// reporting the holder's `.incomplete` size through `progress` meanwhile.
async fn acquire_blob_lock<F>(
    lock_path: &Path,
    blob_path: &Path,
    incomplete_path: &Path,
    total: u64,
    progress: &mut F,
) -> Result<LockOutcome>
where
    // The return is ignored: this only mirrors another process's progress.
    F: FnMut(u64, u64) -> bool,
{
    use fs2::FileExt as _;
    use std::fs::OpenOptions;

    if let Some(parent) = lock_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    let open_lock = || -> Result<std::fs::File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .with_context(|| format!("open lock file {}", lock_path.display()))
    };

    let file = open_lock()?;
    match file.try_lock_exclusive() {
        Ok(()) => return Ok(LockOutcome::Acquired(BlobLockGuard { _file: file })),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(anyhow!("lock {}: {e}", lock_path.display())),
    }
    drop(file);

    for _ in 0..BLOB_LOCK_MAX_POLLS {
        if let Ok(meta) = tokio::fs::metadata(blob_path).await {
            if total == 0 || meta.len() == total {
                progress(meta.len(), if total == 0 { meta.len() } else { total });
                return Ok(LockOutcome::AnotherFinished);
            }
        }
        // Mirror the in-flight download's progress for UI smoothness.
        if let Ok(meta) = tokio::fs::metadata(incomplete_path).await {
            progress(meta.len(), total);
        }
        tokio::time::sleep(BLOB_LOCK_POLL_INTERVAL).await;

        // Did the holder die?
        let file = open_lock()?;
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(LockOutcome::Acquired(BlobLockGuard { _file: file })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                drop(file);
                continue;
            }
            Err(e) => return Err(anyhow!("lock {}: {e}", lock_path.display())),
        }
    }
    Err(anyhow!(
        "timed out waiting for blob lock {} after {}s",
        lock_path.display(),
        BLOB_LOCK_MAX_POLLS as u64 * BLOB_LOCK_POLL_INTERVAL.as_secs()
    ))
}

/// Result of a manually-followed HEAD chain.
#[derive(Debug)]
struct HeadResult {
    final_url: String,
    headers: reqwest::header::HeaderMap,
}

impl HeadResult {
    fn headers(&self) -> &reqwest::header::HeaderMap {
        &self.headers
    }
}

/// HEAD `url`, following redirects by hand so the token only reaches HF/CloudFront hosts.
async fn head_with_redirects(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<HeadResult> {
    let mut current = url.to_string();
    for _ in 0..10 {
        let host = url::Url::parse(&current)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));

        let mut req = client.head(&current);
        if let Some(t) = token {
            if let Some(h) = host.as_deref() {
                if should_send_auth_on_redirect(h) {
                    req = req.bearer_auth(t);
                }
            }
        }
        // Egress-gate every hop, not just `url`: an HF redirect may point at a third-party host.
        let call = pond_core::shared::services::egress::begin(&current, "HEAD")?;
        let sent = req.send().await;
        call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
        let resp = sent.with_context(|| format!("HEAD {current}"))?;

        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect from {current} without Location"))?;
            current = absolute_url(&current, loc)?;
            continue;
        }
        if !status.is_success() {
            return Err(anyhow!("HEAD {current} returned {status}"));
        }
        return Ok(HeadResult {
            final_url: current,
            headers: resp.headers().clone(),
        });
    }
    Err(anyhow!("too many redirects following HEAD {url}"))
}

/// GET `url` following redirects like `head_with_redirects`, re-sending `Range` on every hop.
async fn get_with_redirects(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    range: Option<&str>,
) -> Result<reqwest::Response> {
    let mut current = url.to_string();
    for _ in 0..10 {
        let host = url::Url::parse(&current)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));

        let mut req = client.get(&current);
        if let Some(t) = token {
            if let Some(h) = host.as_deref() {
                if should_send_auth_on_redirect(h) {
                    req = req.bearer_auth(t);
                }
            }
        }
        if let Some(r) = range {
            req = req.header(reqwest::header::RANGE, r);
        }
        // Per hop, as in the HEAD loop; the file-level egress guard can't see a missed site.
        let call = pond_core::shared::services::egress::begin(&current, "GET")?;
        let sent = req.send().await;
        call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
        let resp = sent.with_context(|| format!("GET {current}"))?;
        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect from {current} without Location"))?;
            current = absolute_url(&current, loc)?;
            continue;
        }
        if !status.is_success() && status.as_u16() != 206 {
            return Err(anyhow!("GET {current} returned {status}"));
        }
        return Ok(resp);
    }
    Err(anyhow!("too many redirects following GET {url}"))
}

/// Resolve a possibly relative `Location` header against the current URL.
fn absolute_url(current: &str, location: &str) -> Result<String> {
    let base = url::Url::parse(current).map_err(|e| anyhow!("parse {current}: {e}"))?;
    let joined = base
        .join(location)
        .map_err(|e| anyhow!("join {location} onto {current}: {e}"))?;
    Ok(joined.into())
}

/// Write `refs/{branch}` and create the snapshot symlink pointing at the blob.
async fn finalize_pointers(
    fetch: &HfFetch<'_>,
    _etag: &str,
    commit: &str,
    blob_path: &Path,
) -> Result<()> {
    let refs_path = fetch.repo.refs_path("main");
    if let Some(parent) = refs_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tokio::fs::write(&refs_path, commit.as_bytes())
        .await
        .with_context(|| format!("write {}", refs_path.display()))?;

    let pointer = fetch.pointer_path(commit);
    if let Some(parent) = pointer.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    // Best-effort cleanup of stale pointer (symlink or file) before recreating.
    let _ = tokio::fs::remove_file(&pointer).await;

    let blob = blob_path.to_path_buf();
    let ptr = pointer.clone();
    tokio::task::spawn_blocking(move || create_pointer(&blob, &ptr))
        .await
        .map_err(|e| anyhow!("pointer task panicked: {e}"))??;
    Ok(())
}

#[cfg(unix)]
fn create_pointer(blob: &Path, pointer: &Path) -> Result<()> {
    std::os::unix::fs::symlink(blob, pointer)
        .with_context(|| format!("symlink {} -> {}", pointer.display(), blob.display()))
}

#[cfg(not(unix))]
fn create_pointer(blob: &Path, pointer: &Path) -> Result<()> {
    std::fs::copy(blob, pointer)
        .map(|_| ())
        .with_context(|| format!("copy {} -> {}", blob.display(), pointer.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    // Env var manipulation must be serialised — Rust tests run in parallel.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Snapshot + clear the env vars this module reads, restoring on drop.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let vars = [
                "HF_HOME",
                "HF_TOKEN",
                "HUGGING_FACE_HUB_TOKEN",
                "HUGGINGFACE_TOKEN",
            ];
            let saved: Vec<_> = vars.iter().map(|v| (*v, std::env::var_os(v))).collect();
            for (v, _) in &saved {
                std::env::remove_var(v);
            }
            Self { saved, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (v, old) in &self.saved {
                match old {
                    Some(val) => std::env::set_var(v, val),
                    None => std::env::remove_var(v),
                }
            }
        }
    }

    #[test]
    fn folder_name_encodes_slashes() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("bartowski/gemma-4-E2B-it-GGUF");
        assert_eq!(repo.folder_name(), "models--bartowski--gemma-4-E2B-it-GGUF");
    }

    #[test]
    fn folder_name_for_simple_repo() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("gpt2");
        assert_eq!(repo.folder_name(), "models--gpt2");
    }

    #[test]
    fn default_root_under_data_dir() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.root(), tmp.path().join("hf_cache"));
        assert_eq!(cache.hub_dir(), tmp.path().join("hf_cache").join("hub"));
    }

    #[test]
    fn hf_home_env_overrides_default() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let override_root = tmp.path().join("custom_hf");
        std::env::set_var("HF_HOME", &override_root);
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.root(), override_root.as_path());
        assert_eq!(cache.hub_dir(), override_root.join("hub"));
    }

    #[test]
    fn token_file_is_read() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("hf_cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("token"), "  hf_abc123\n").unwrap();
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.token(), Some("hf_abc123"));
    }

    #[test]
    fn empty_token_file_returns_none() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("hf_cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("token"), "   \n  \t  ").unwrap();
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.token(), None);
    }

    #[test]
    fn env_var_takes_precedence_over_token_file() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("hf_cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("token"), "from_file").unwrap();
        std::env::set_var("HF_TOKEN", "from_env");
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.token(), Some("from_env"));
    }

    #[test]
    fn url_with_default_revision() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("bartowski/foo");
        let fetch = repo.file("bar.gguf");
        assert_eq!(
            fetch.url(),
            "https://huggingface.co/bartowski/foo/resolve/main/bar.gguf"
        );
    }

    #[test]
    fn url_with_branch_slashes_url_escapes() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("bartowski/foo").with_revision("feature/x");
        let fetch = repo.file("bar.gguf");
        assert_eq!(
            fetch.url(),
            "https://huggingface.co/bartowski/foo/resolve/feature%2Fx/bar.gguf"
        );
    }

    #[test]
    fn parse_hf_url_simple() {
        let (repo, rev, file) =
            parse_hf_url("https://huggingface.co/bartowski/foo/resolve/main/bar.gguf").unwrap();
        assert_eq!(repo, "bartowski/foo");
        assert_eq!(rev, "main");
        assert_eq!(file, "bar.gguf");
    }

    #[test]
    fn parse_hf_url_subfolder_file() {
        let (repo, rev, file) = parse_hf_url(
            "https://huggingface.co/immich-app/antelopev2/resolve/main/recognition/model.onnx",
        )
        .unwrap();
        assert_eq!(repo, "immich-app/antelopev2");
        assert_eq!(rev, "main");
        assert_eq!(file, "recognition/model.onnx");
    }

    #[test]
    fn parse_hf_url_revision_with_encoded_slash() {
        let (repo, rev, file) =
            parse_hf_url("https://huggingface.co/foo/bar/resolve/refs%2Fpr%2F123/model.gguf")
                .unwrap();
        assert_eq!(repo, "foo/bar");
        assert_eq!(rev, "refs/pr/123");
        assert_eq!(file, "model.gguf");
    }

    #[test]
    fn parse_hf_url_non_hf_returns_none() {
        assert!(parse_hf_url("https://example.com/foo/bar/resolve/main/x.bin").is_none());
        assert!(parse_hf_url("https://github.com/owner/repo/releases/download/v1/x").is_none());
    }

    #[test]
    fn parse_hf_url_missing_segments_returns_none() {
        assert!(parse_hf_url("https://huggingface.co/foo/bar").is_none());
        assert!(parse_hf_url("https://huggingface.co/foo/bar/resolve/main/").is_none());
        assert!(parse_hf_url("https://huggingface.co/foo/bar/blob/main/x").is_none());
    }

    #[test]
    fn auth_redirect_policy_allows_hf_and_cdn_hosts() {
        assert!(should_send_auth_on_redirect("huggingface.co"));
        assert!(should_send_auth_on_redirect("cdn-lfs.huggingface.co"));
        assert!(should_send_auth_on_redirect(
            "d2l4uplgqnwxzd.cloudfront.net"
        ));
        assert!(should_send_auth_on_redirect("HUGGINGFACE.CO"));
    }

    #[test]
    fn auth_redirect_policy_blocks_other_hosts() {
        assert!(!should_send_auth_on_redirect("evil.com"));
        assert!(!should_send_auth_on_redirect("example.org"));
        assert!(!should_send_auth_on_redirect("127.0.0.1"));
        assert!(!should_send_auth_on_redirect("localhost"));
        // Substring trickery: "huggingface.co.evil.com" is NOT an HF host.
        assert!(!should_send_auth_on_redirect("huggingface.co.evil.com"));
    }

    #[test]
    fn blob_and_pointer_paths_join_correctly() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("bartowski/gemma-4-E2B-it-GGUF");
        let folder = tmp
            .path()
            .join("hf_cache")
            .join("hub")
            .join("models--bartowski--gemma-4-E2B-it-GGUF");

        assert_eq!(repo.folder_path(), folder);
        assert_eq!(repo.refs_path("main"), folder.join("refs").join("main"));
        assert_eq!(
            repo.snapshot_dir("abc123"),
            folder.join("snapshots").join("abc123")
        );
        assert_eq!(
            repo.blob_path("deadbeef"),
            folder.join("blobs").join("deadbeef")
        );

        let fetch = repo.file("gemma-4-E2B-it-Q4_K_M.gguf");
        assert_eq!(
            fetch.pointer_path("abc123"),
            folder
                .join("snapshots")
                .join("abc123")
                .join("gemma-4-E2B-it-Q4_K_M.gguf")
        );
        assert_eq!(
            fetch.blob_path("deadbeef"),
            folder.join("blobs").join("deadbeef")
        );
    }

    // ── Network-mode gate, per redirect hop ─────────────────────────────────
    // Behavioural, one test per site: `egress_guard.rs` only checks the file mentions a tracker.
    // `network_mode` is process-global; a test wanting a different mode needs serialising.

    use pond_core::shared::services::egress::{network_mode, set_network_mode, NetworkMode};
    use wiremock::matchers::{method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Restores `NetworkMode::Open` on drop, so a panicking test can't leave the binary offline.
    struct ModeGuard(NetworkMode);

    impl ModeGuard {
        fn set(mode: NetworkMode) -> Self {
            let previous = network_mode();
            set_network_mode(mode);
            Self(previous)
        }
    }

    impl Drop for ModeGuard {
        fn drop(&mut self) {
            set_network_mode(self.0);
        }
    }

    /// A loopback server that 302s to `location`.
    async fn redirector(location: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(wm_method("HEAD"))
            .and(wm_path("/start"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location))
            .mount(&server)
            .await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/start"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn head_redirect_to_a_non_loopback_host_is_refused_at_the_hop() {
        // `.invalid` never resolves (RFC 2606), so a missed gate fails with a DNS error instead.
        let server = redirector("https://cdn.invalid/blob").await;
        let _mode = ModeGuard::set(NetworkMode::Allowlist);

        let client = build_redirect_aware_client(None).expect("client builds");
        let err = head_with_redirects(&client, &format!("{}/start", server.uri()), None)
            .await
            .expect_err("the second hop leaves loopback and must be refused");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("cdn.invalid"),
            "the refusal must name the host it refused: {rendered}"
        );
        assert!(
            rendered.contains("network_mode") && rendered.contains("allowlist"),
            "the refusal must name the setting and its value, or it is \
             indistinguishable from the network being down: {rendered}"
        );
    }

    #[tokio::test]
    async fn get_redirect_to_a_non_loopback_host_is_refused_at_the_hop() {
        let server = redirector("https://cdn.invalid/blob").await;
        let _mode = ModeGuard::set(NetworkMode::Allowlist);

        let client = build_redirect_aware_client(None).expect("client builds");
        let err = get_with_redirects(&client, &format!("{}/start", server.uri()), None, None)
            .await
            .expect_err("the second hop leaves loopback and must be refused");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("cdn.invalid"),
            "the refusal must name the host it refused: {rendered}"
        );
        assert!(
            rendered.contains("network_mode") && rendered.contains("allowlist"),
            "the refusal must name the setting and its value: {rendered}"
        );
    }

    /// Vacuity control: a gate refusing every hop would pass the two tests above.
    #[tokio::test]
    async fn a_permitted_redirect_chain_still_completes() {
        let destination = MockServer::start().await;
        Mock::given(wm_method("HEAD"))
            .and(wm_path("/blob"))
            .respond_with(ResponseTemplate::new(200).insert_header("etag", "\"deadbeef\""))
            .mount(&destination)
            .await;
        let hop = format!("{}/blob", destination.uri());
        let server = redirector(&hop).await;
        let _mode = ModeGuard::set(NetworkMode::Allowlist);

        let client = build_redirect_aware_client(None).expect("client builds");
        let head = head_with_redirects(&client, &format!("{}/start", server.uri()), None)
            .await
            .expect("loopback to loopback is permitted under allowlist");

        assert_eq!(head.final_url, hop);
        assert_eq!(
            head.headers().get("etag").and_then(|v| v.to_str().ok()),
            Some("\"deadbeef\"")
        );
    }
}
