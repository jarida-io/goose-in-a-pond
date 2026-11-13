//! Runs a headless browser once to read a page that scripts build, and finds or downloads the
//! browser. One render at a time, never without room beside the model, and nothing is kept
//! between runs.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use pond_core::shared::domain::page_render::{
    self as rules, NotRendered, BROWSER_NAMES, OWN_BROWSER_DIR, OWN_BROWSER_NAME,
    RENDERED_BYTE_CAP, RENDER_TIMEOUT_SECS,
};
use pond_core::shared::ports::browser::{Browser, Install, Status};
use tokio::io::AsyncReadExt;

/// Where the pond keeps its own data; the browser it downloads goes under it.
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Install the data directory once, at startup; later calls are ignored.
pub fn init_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(dir);
}

static RENDERING: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(1));

static INSTALL: LazyLock<Mutex<Install>> = LazyLock::new(|| Mutex::new(Install::Idle));

fn set_install(state: Install) {
    *INSTALL.lock().unwrap_or_else(|e| e.into_inner()) = state;
}

pub fn install_state() -> Install {
    INSTALL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The pond's own downloaded browser, if it is there.
fn own_browser() -> Option<PathBuf> {
    let dir = DATA_DIR.get()?.join(OWN_BROWSER_DIR);
    let direct = dir.join(OWN_BROWSER_NAME);
    if direct.is_file() {
        return Some(direct);
    }
    // The publisher's archive unpacks into one folder named for the platform.
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(OWN_BROWSER_NAME))
        .find(|path| path.is_file())
}

fn on_search_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    })
}

/// The browser a render would use: the pond's own, else one installed on the machine.
pub fn find_browser() -> Option<PathBuf> {
    own_browser()
        .or_else(|| BROWSER_NAMES.iter().find_map(|name| on_search_path(name)))
        .or_else(|| {
            rules::application_paths()
                .into_iter()
                .find(|path| path.is_file())
        })
}

/// Memory the machine could give a new process, in MB; `None` where it does not say.
pub fn free_memory_mb() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// Whether a page that needs a browser could be rendered now, and why not.
pub fn readiness() -> Result<PathBuf, NotRendered> {
    if !rules::is_enabled() {
        return Err(NotRendered::Off);
    }
    let browser = find_browser().ok_or(NotRendered::NoBrowser)?;
    rules::room_to_render(free_memory_mb())?;
    Ok(browser)
}

/// The page at `address` as a browser builds it: its HTML once its scripts have run.
pub async fn render(address: &str) -> Result<String, NotRendered> {
    let browser = readiness()?;
    let Ok(_one_at_a_time) = RENDERING.try_acquire() else {
        return Err(NotRendered::Busy);
    };
    let profile = std::env::temp_dir().join(format!("pond-render-{}", uuid::Uuid::new_v4()));
    let started = std::time::Instant::now();
    let outcome = run(&browser, address, &profile).await;
    let _ = tokio::fs::remove_dir_all(&profile).await;
    tracing::info!(
        target: "giap::trace",
        kind = "web_page_rendered",
        ok = outcome.is_ok(),
        bytes = outcome.as_ref().map(String::len).unwrap_or(0),
        ms = started.elapsed().as_millis() as u64,
        "a page was read with the browser"
    );
    outcome
}

async fn run(browser: &Path, address: &str, profile: &Path) -> Result<String, NotRendered> {
    let mut child = tokio::process::Command::new(browser)
        .args(rules::render_args(address, profile))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| NotRendered::Failed(format!("the browser did not start: {e}")))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| NotRendered::Failed("the browser gave no output".to_string()))?;
    let read = async {
        let mut html = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match stdout.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    html.extend_from_slice(&chunk[..n]);
                    if html.len() >= RENDERED_BYTE_CAP {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        html
    };
    let html = tokio::time::timeout(Duration::from_secs(RENDER_TIMEOUT_SECS), read).await;
    // Dropping the child kills it; wait so it is reaped before the profile is removed.
    let _ = child.kill().await;
    let html = html.map_err(|_| NotRendered::Failed("the browser took too long".to_string()))?;
    let html = String::from_utf8_lossy(&html).into_owned();
    if html.trim().is_empty() {
        return Err(NotRendered::Failed(
            "the browser gave an empty page".to_string(),
        ));
    }
    Ok(html)
}

/// `html` as fetched, or as a browser builds it when it is an unfilled shell. A shell that
/// could not be rendered is the reason why.
pub async fn filled(address: &str, html: String) -> Result<String, NotRendered> {
    let readable = rules::rough_text_chars(&html);
    if !rules::needs_browser(&html, readable) {
        return Ok(html);
    }
    match render(address).await {
        Ok(rendered) if rules::rough_text_chars(&rendered) > readable => Ok(rendered),
        Ok(_) => Err(NotRendered::Failed(
            "the browser found no more to read".to_string(),
        )),
        Err(why) => {
            tracing::info!(
                target: "giap::trace",
                kind = "web_page_not_rendered",
                reason = why.reason(),
                "a page needed a browser and was not rendered"
            );
            Err(why)
        }
    }
}

pub fn status() -> Status {
    let browser = find_browser();
    let ready = readiness();
    Status {
        enabled: rules::is_enabled(),
        own: browser.is_some() && browser == own_browser(),
        browser: browser.map(|p| p.display().to_string()),
        free_mb: free_memory_mb(),
        needs_free_mb: rules::MIN_FREE_MB,
        ready: ready.is_ok(),
        reason: ready.err().map(|why| why.reason()),
        can_download: rules::download_platform().is_some() && DATA_DIR.get().is_some(),
        install: install_state(),
    }
}

const VERSIONS: &str =
    "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";
/// The largest archive taken: the shell is about 130 MB.
const ARCHIVE_BYTE_CAP: u64 = 400 * 1024 * 1024;

/// The stable headless shell's archive for this machine, from the publisher's own list.
async fn archive_address(client: &reqwest::Client) -> Result<String, String> {
    let platform = rules::download_platform().ok_or("no browser is published for this machine")?;
    let response =
        crate::http::traced_get_with(client, VERSIONS, |b| b.timeout(Duration::from_secs(30)))
            .await
            .map_err(|e| format!("the list of browsers could not be read: {e}"))?;
    let list: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("the list of browsers could not be read: {e}"))?;
    list["channels"]["Stable"]["downloads"]["chrome-headless-shell"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["platform"] == platform)
        .and_then(|entry| entry["url"].as_str())
        .filter(|url| url.starts_with("https://storage.googleapis.com/chrome-for-testing-public/"))
        .map(str::to_string)
        .ok_or_else(|| "the list of browsers names none for this machine".to_string())
}

/// Download the headless shell into the data directory and check that it runs. The household
/// asked for this in Settings; it is about 130 MB.
pub async fn install() -> Result<PathBuf, String> {
    if matches!(
        install_state(),
        Install::Downloading { .. } | Install::Unpacking
    ) {
        return Err("a browser is already being downloaded".to_string());
    }
    let outcome = install_inner().await;
    match &outcome {
        Ok(_) => set_install(Install::Idle),
        Err(why) => set_install(Install::Failed { why: why.clone() }),
    }
    outcome
}

async fn install_inner() -> Result<PathBuf, String> {
    let dir = DATA_DIR
        .get()
        .ok_or("the pond has no data directory to keep a browser in")?
        .join(OWN_BROWSER_DIR);
    set_install(Install::Downloading {
        done_bytes: 0,
        total_bytes: None,
    });
    let client = crate::web_search::page_client().ok_or("the web client could not be built")?;
    let address = archive_address(client).await?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("the browser folder could not be made: {e}"))?;
    let archive = dir.join("download.zip");
    let mut response =
        crate::http::traced_get_with(client, &address, |b| b.timeout(Duration::from_secs(900)))
            .await
            .map_err(|e| format!("the download did not start: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("the download answered HTTP {}", response.status()));
    }
    let total_bytes = response.content_length();
    let mut file = tokio::fs::File::create(&archive)
        .await
        .map_err(|e| format!("the download could not be saved: {e}"))?;
    let mut done_bytes = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("the download stopped: {e}"))?
    {
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| format!("the download could not be saved: {e}"))?;
        done_bytes += chunk.len() as u64;
        if done_bytes > ARCHIVE_BYTE_CAP {
            let _ = tokio::fs::remove_file(&archive).await;
            return Err("the download was larger than a browser should be".to_string());
        }
        set_install(Install::Downloading {
            done_bytes,
            total_bytes,
        });
    }
    drop(file);

    set_install(Install::Unpacking);
    let unpacked = tokio::process::Command::new("unzip")
        .arg("-q")
        .arg("-o")
        .arg(&archive)
        .arg("-d")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|e| format!("the archive could not be unpacked (unzip: {e})"))?;
    let _ = tokio::fs::remove_file(&archive).await;
    if !unpacked.success() {
        return Err("the archive could not be unpacked".to_string());
    }
    let browser = own_browser().ok_or("the archive held no browser")?;
    let runs = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(&browser)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
    match runs {
        Ok(Ok(status)) if status.success() => {
            tracing::info!(target: "giap::trace", kind = "browser_installed", bytes = done_bytes);
            Ok(browser)
        }
        _ => {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            Err("the downloaded browser does not run on this machine".to_string())
        }
    }
}

/// Remove the pond's own browser; one installed on the machine is not touched.
pub async fn uninstall() -> bool {
    let Some(dir) = DATA_DIR.get().map(|d| d.join(OWN_BROWSER_DIR)) else {
        return false;
    };
    tokio::fs::remove_dir_all(&dir).await.is_ok()
}

/// This module as the port the settings routes reach.
pub struct PondBrowser;

#[async_trait::async_trait]
impl Browser for PondBrowser {
    fn status(&self) -> Status {
        status()
    }

    async fn install(&self) -> Result<(), String> {
        install().await.map(|_| ())
    }

    async fn uninstall(&self) -> bool {
        uninstall().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_page_with_words_is_never_sent_to_a_browser() {
        let html = format!("<html><body><p>{}</p></body></html>", "word ".repeat(80));
        assert_eq!(filled("https://example.com", html.clone()).await, Ok(html));
    }

    /// Drives the machine's real browser; a machine with none has nothing to check.
    #[tokio::test]
    async fn a_page_built_by_a_script_is_read_as_the_browser_builds_it() {
        if find_browser().is_none() || free_memory_mb().is_some_and(|mb| mb < rules::MIN_FREE_MB) {
            eprintln!("no browser, or no room for one: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("pond-render-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("app.html");
        std::fs::write(
            &page,
            "<html><body><div id=\"root\"></div><script>\
             document.getElementById('root').innerHTML = '<h1>Opening hours</h1><p>' + \
             ['Monday to Friday', 'nine', 'to', 'five'].join(' ') + '. ' + \
             'Closed on public holidays and on the last Sunday of every month, when the \
             building is cleaned from top to bottom and nobody is let in before noon. \
             Deliveries are taken at the side door on Tuesdays and Thursdays only, and \
             the front desk answers the telephone until half past four.</p>';\
             </script></body></html>",
        )
        .unwrap();
        let address = format!("file://{}", page.display());
        let shell = std::fs::read_to_string(&page).unwrap();
        assert!(rules::needs_browser(
            &shell,
            rules::rough_text_chars(&shell)
        ));
        let rendered = filled(&address, shell).await.expect("the browser reads it");
        assert!(
            rendered.contains("Monday to Friday nine to five"),
            "{rendered}"
        );
        assert!(!rules::needs_browser(
            &rendered,
            rules::rough_text_chars(&rendered)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_shell_is_not_rendered_while_the_household_has_it_off() {
        rules::set_enabled(false);
        let shell =
            "<html><body><div id=\"root\"></div><script src=\"a.js\"></script></body></html>";
        let outcome = filled("https://example.com", shell.to_string()).await;
        rules::set_enabled(true);
        assert_eq!(outcome, Err(NotRendered::Off));
    }
}
