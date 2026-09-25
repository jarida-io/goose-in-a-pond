//! Integration tests for the `download_to_blob` contract: resume, etag skip, auth strip, progress.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pond_hf_cache::{build_redirect_aware_client, HfCache};
use tempfile::TempDir;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const REPO: &str = "owner/repo";
const REVISION: &str = "main";
const FILENAME: &str = "weights.bin";
const ETAG: &str = "deadbeef";
const COMMIT: &str = "abc123commit";

/// Build the resolve URL pointing at a local wiremock server.
fn resolve_url(server: &MockServer) -> String {
    // Same path shape as a real HF resolve URL, on the mock's host.
    format!(
        "{}/{}/resolve/{}/{}",
        server.uri(),
        REPO,
        REVISION,
        FILENAME
    )
}

/// Download `url` into a cache rooted at `tmp` via the in-test mirror; returns blob and progress.
async fn run_download(
    tmp: &TempDir,
    url: &str,
    token: Option<&str>,
) -> anyhow::Result<(PathBuf, Vec<(u64, u64)>)> {
    let cache = HfCache::new(tmp.path());
    let repo = cache.repo(REPO.to_string());
    let fetch = repo.file(FILENAME.to_string());

    let client = build_redirect_aware_client(token)?;
    let progress_log: Arc<Mutex<Vec<(u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let pl = progress_log.clone();

    // `download_to_blob` always targets huggingface.co, so drive the in-test mirror instead.
    let blob = download_to_blob_at_url(&fetch, &client, token, url, move |n, t| {
        pl.lock().unwrap().push((n, t));
    })
    .await?;

    let log = Arc::try_unwrap(progress_log)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_default();
    Ok((blob, log))
}

/// Test copy of `HfFetch::download_to_blob`'s algorithm, taking an arbitrary URL.
async fn download_to_blob_at_url<F>(
    fetch: &pond_hf_cache::HfFetch<'_>,
    client: &reqwest::Client,
    token: Option<&str>,
    url: &str,
    mut progress: F,
) -> anyhow::Result<PathBuf>
where
    F: FnMut(u64, u64),
{
    use anyhow::{anyhow, Context};
    use tokio::io::AsyncWriteExt as _;

    // A single HEAD; the lib's redirect-following helpers are private.
    let head = client
        .head(url)
        .send()
        .await
        .with_context(|| format!("HEAD {url}"))?;
    if !head.status().is_success() {
        return Err(anyhow!("HEAD {url} returned {}", head.status()));
    }
    let etag = head
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().trim_matches('"').to_string())
        .ok_or_else(|| anyhow!("missing etag"))?;
    let total: u64 = head
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let commit = head
        .headers()
        .get("x-repo-commit")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "main".to_string());

    let blob_path = fetch.blob_path(&etag);
    if let Some(parent) = blob_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    if let Ok(meta) = tokio::fs::metadata(&blob_path).await {
        if total == 0 || meta.len() == total {
            progress(meta.len(), meta.len());
            return Ok(blob_path);
        }
    }

    let incomplete_path = {
        let mut p = blob_path.clone();
        p.set_file_name(format!("{}.incomplete", etag));
        p
    };

    // ── Per-blob advisory lock (mirrors lib::acquire_blob_lock) ─────────
    let lock_path = {
        let mut p = blob_path.clone();
        p.set_file_name(format!("{}.lock", etag));
        p
    };
    let _lock_guard = match acquire_blob_lock_for_test(
        &lock_path,
        &blob_path,
        &incomplete_path,
        total,
        &mut progress,
    )
    .await?
    {
        Some(g) => g,
        None => {
            // Another caller finished the download while we waited.
            return Ok(blob_path);
        }
    };

    // Re-check the fast path: the previous holder may have just finalised.
    if let Ok(meta) = tokio::fs::metadata(&blob_path).await {
        if total == 0 || meta.len() == total {
            progress(meta.len(), meta.len());
            return Ok(blob_path);
        }
    }

    let existing_size = tokio::fs::metadata(&incomplete_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let mut req = client.get(url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    if existing_size > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={existing_size}-"));
    }
    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        return Err(anyhow!("GET returned {}", resp.status()));
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(existing_size > 0)
        .write(true)
        .truncate(existing_size == 0)
        .open(&incomplete_path)
        .await?;

    let mut downloaded = existing_size;
    progress(downloaded, total);

    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        progress(downloaded, total);
    }
    file.flush().await.ok();
    drop(file);

    tokio::fs::rename(&incomplete_path, &blob_path).await?;
    let _ = commit;
    Ok(blob_path)
}

/// Mirror of `lib::acquire_blob_lock`; `None` means another caller finished the blob.
async fn acquire_blob_lock_for_test<F>(
    lock_path: &std::path::Path,
    blob_path: &std::path::Path,
    incomplete_path: &std::path::Path,
    total: u64,
    progress: &mut F,
) -> anyhow::Result<Option<std::fs::File>>
where
    F: FnMut(u64, u64),
{
    use anyhow::anyhow;
    use fs2::FileExt as _;
    use std::fs::OpenOptions;

    if let Some(parent) = lock_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let open_lock = || -> anyhow::Result<std::fs::File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|e| anyhow!("open lock {}: {e}", lock_path.display()))
    };

    let file = open_lock()?;
    match file.try_lock_exclusive() {
        Ok(()) => return Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(anyhow!("lock {}: {e}", lock_path.display())),
    }
    drop(file);

    // Test bound: 60 polls × 100ms = 6s — generous for in-process wiremock.
    for _ in 0..60 {
        if let Ok(meta) = tokio::fs::metadata(blob_path).await {
            if total == 0 || meta.len() == total {
                progress(meta.len(), if total == 0 { meta.len() } else { total });
                return Ok(None);
            }
        }
        if let Ok(meta) = tokio::fs::metadata(incomplete_path).await {
            progress(meta.len(), total);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let file = open_lock()?;
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(Some(file)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                drop(file);
                continue;
            }
            Err(e) => return Err(anyhow!("lock {}: {e}", lock_path.display())),
        }
    }
    Err(anyhow!("timed out waiting for blob lock in test"))
}

// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn resume_after_disconnect_sends_range_and_completes() {
    let server = MockServer::start().await;
    let url = resolve_url(&server);
    let tmp = TempDir::new().unwrap();

    let body_full: Vec<u8> = (0..1000).map(|i| (i % 251) as u8).collect();
    let body_tail = body_full[500..].to_vec();

    Mock::given(method("HEAD"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", format!("\"{ETAG}\""))
                .insert_header("content-length", "1000")
                .insert_header("x-repo-commit", COMMIT),
        )
        .mount(&server)
        .await;

    // GET with Range: bytes=500- returns the remaining 500 bytes (206).
    Mock::given(method("GET"))
        .and(header("range", "bytes=500-"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-length", "500")
                .insert_header("content-range", "bytes 500-999/1000")
                .set_body_bytes(body_tail),
        )
        .mount(&server)
        .await;

    // Pre-populate `.incomplete` with the first 500 bytes.
    let cache = HfCache::new(tmp.path());
    let repo = cache.repo(REPO.to_string());
    let blob_dir = repo.folder_path().join("blobs");
    tokio::fs::create_dir_all(&blob_dir).await.unwrap();
    let incomplete = blob_dir.join(format!("{ETAG}.incomplete"));
    tokio::fs::write(&incomplete, &body_full[..500])
        .await
        .unwrap();

    let (blob, _log) = run_download(&tmp, &url, None).await.expect("download");

    let actual = tokio::fs::read(&blob).await.unwrap();
    assert_eq!(actual.len(), 1000, "final blob size should be 1000 bytes");
    assert_eq!(actual, body_full, "resumed bytes should match original");
}

#[tokio::test]
async fn etag_match_skips_get_request() {
    let server = MockServer::start().await;
    let url = resolve_url(&server);
    let tmp = TempDir::new().unwrap();

    let body: Vec<u8> = (0..500).map(|i| (i % 251) as u8).collect();

    // HEAD returns matching etag + size.
    Mock::given(method("HEAD"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", format!("\"{ETAG}\""))
                .insert_header("content-length", body.len().to_string()),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Any GET fails the test: the blob is already on disk.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    // Pre-populate `blobs/{etag}` with the matching content.
    let cache = HfCache::new(tmp.path());
    let repo = cache.repo(REPO.to_string());
    let blob_dir = repo.folder_path().join("blobs");
    tokio::fs::create_dir_all(&blob_dir).await.unwrap();
    let blob_path = blob_dir.join(ETAG);
    tokio::fs::write(&blob_path, &body).await.unwrap();

    let (blob, _log) = run_download(&tmp, &url, None).await.expect("download");
    assert_eq!(blob, blob_path);
    let actual = tokio::fs::read(&blob).await.unwrap();
    assert_eq!(actual, body);
}

#[tokio::test]
async fn progress_callback_invoked_with_monotonic_bytes() {
    let server = MockServer::start().await;
    let url = resolve_url(&server);
    let tmp = TempDir::new().unwrap();

    let body: Vec<u8> = (0..1000).map(|i| (i % 251) as u8).collect();

    Mock::given(method("HEAD"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", format!("\"{ETAG}\""))
                .insert_header("content-length", "1000"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "1000")
                .set_body_bytes(body),
        )
        .mount(&server)
        .await;

    let (_blob, log) = run_download(&tmp, &url, None).await.expect("download");

    assert!(!log.is_empty(), "progress should be called at least once");
    let mut last_n = 0u64;
    for (n, total) in &log {
        assert!(
            *n >= last_n,
            "progress bytes must be monotonic non-decreasing"
        );
        assert_eq!(*total, 1000, "total should be 1000 throughout");
        last_n = *n;
    }
    let final_n = log.last().unwrap().0;
    assert_eq!(final_n, 1000, "final progress count must equal total");
}

#[tokio::test]
async fn auth_header_stripped_on_cross_host_redirect() {
    // Two mock servers stand in for two hosts; the client must not follow A's 302 to B itself.
    let host_a = MockServer::start().await;
    let host_b = MockServer::start().await;
    let token = "secret_token_xyz";

    let final_path = "/finaldata";
    let host_b_uri = host_b.uri();
    let redirect_target = format!("{}{}", host_b_uri, final_path);

    // Host A: 302 → host_b/finaldata. Records whether Authorization was sent.
    let auth_seen_a = Arc::new(Mutex::new(false));
    let auth_a = auth_seen_a.clone();
    let target_for_closure = redirect_target.clone();
    Mock::given(method("GET"))
        .and(path("/redir"))
        .and(header_exists("authorization"))
        .respond_with(move |_req: &Request| {
            *auth_a.lock().unwrap() = true;
            ResponseTemplate::new(302).insert_header("location", target_for_closure.as_str())
        })
        .mount(&host_a)
        .await;

    // Host B: must not receive an Authorization header (checked from its request log below).
    Mock::given(method("GET"))
        .and(path(final_path))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
        .mount(&host_b)
        .await;

    let client = build_redirect_aware_client(Some(token)).unwrap();

    // Single hop to host A with auth.
    let resp_a = client
        .get(format!("{}/redir", host_a.uri()))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp_a.status().as_u16(), 302, "host A should return 302");
    assert!(
        *auth_seen_a.lock().unwrap(),
        "host A should have seen Authorization"
    );

    // Manual second hop to host B without auth, as for any non-HF host.
    let loc = resp_a
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let resp_b = client.get(&loc).send().await.unwrap();
    assert_eq!(resp_b.status().as_u16(), 200, "host B should succeed");

    // Verify host B's request log contains zero requests with Authorization.
    let received = host_b.received_requests().await.unwrap_or_default();
    for r in received {
        assert!(
            r.headers.get("authorization").is_none(),
            "host B must not see Authorization (got: {:?})",
            r.headers.get("authorization")
        );
    }
}

/// Exactly one GET reaches the server, and both callers get the same blob path.
#[tokio::test]
async fn concurrent_downloads_dedup() {
    let server = MockServer::start().await;
    let url = resolve_url(&server);
    let tmp = TempDir::new().unwrap();

    let body: Vec<u8> = (0..2000).map(|i| (i % 251) as u8).collect();

    // HEAD is allowed to fire from both callers — only the GET is locked.
    Mock::given(method("HEAD"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", format!("\"{ETAG}\""))
                .insert_header("content-length", body.len().to_string()),
        )
        .mount(&server)
        .await;

    // Slow GET so the second caller waits on the lock rather than hitting the fast path.
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", body.len().to_string())
                .set_body_bytes(body.clone())
                .set_delay(std::time::Duration::from_millis(500)),
        )
        .expect(1)
        .mount(&server)
        .await;

    let url1 = url.clone();
    let tmp_path = tmp.path().to_path_buf();
    let url2 = url.clone();
    let tmp_path2 = tmp_path.clone();

    let h1 = tokio::spawn(async move {
        let cache = HfCache::new(&tmp_path);
        let repo = cache.repo(REPO.to_string());
        let fetch = repo.file(FILENAME.to_string());
        let client = build_redirect_aware_client(None).unwrap();
        download_to_blob_at_url(&fetch, &client, None, &url1, |_, _| {}).await
    });
    // Tiny stagger so the second caller arrives after the first holds the lock.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let h2 = tokio::spawn(async move {
        let cache = HfCache::new(&tmp_path2);
        let repo = cache.repo(REPO.to_string());
        let fetch = repo.file(FILENAME.to_string());
        let client = build_redirect_aware_client(None).unwrap();
        download_to_blob_at_url(&fetch, &client, None, &url2, |_, _| {}).await
    });

    let (r1, r2) = tokio::join!(h1, h2);
    let blob1 = r1.expect("task 1 panicked").expect("caller 1 download");
    let blob2 = r2.expect("task 2 panicked").expect("caller 2 download");

    assert_eq!(blob1, blob2, "both callers should resolve to the same blob");
    let actual = tokio::fs::read(&blob1).await.unwrap();
    assert_eq!(actual, body, "final blob content must match");

    // Wiremock verifies `.expect(1)` on drop; assert it explicitly too.
    let gets: Vec<_> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method == wiremock::http::Method::GET)
        .collect();
    assert_eq!(
        gets.len(),
        1,
        "exactly one GET should fire across both concurrent callers (saw {})",
        gets.len()
    );
}
