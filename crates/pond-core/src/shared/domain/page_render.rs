//! Reading a page that only a browser can show. Most pages arrive as words; some arrive as an
//! empty shell that scripts fill in. For those the pond runs a headless browser once, takes the
//! page as the browser built it, and reads that with its usual reader. Nothing here starts a
//! process: these are the rules for when to, with what, and why not.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Readable characters below which a page with scripts is taken for an unfilled shell.
pub const SHELL_TEXT_CHARS: usize = 200;
/// Memory the board must have free before a browser is started beside the model, in MB: on
/// the Orin Nano a render took up to 307 MB, and 300 MB stays for the pond.
pub const MIN_FREE_MB: u64 = 600;
/// How long one render may take, start to finish.
pub const RENDER_TIMEOUT_SECS: u64 = 30;
/// How long the page's own scripts are given to finish, in the browser's virtual time.
pub const SCRIPT_BUDGET_MS: u32 = 8_000;
/// The most of a rendered page that is read, in bytes.
pub const RENDERED_BYTE_CAP: usize = 4 * 1024 * 1024;

static ENABLED: AtomicBool = AtomicBool::new(true);

/// The household's switch, set at startup and whenever the setting changes.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Why a page that needed a browser was not rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotRendered {
    /// The household turned browser reading off.
    Off,
    /// No browser is installed and the pond has not downloaded one.
    NoBrowser,
    /// Too little memory is free to start a browser beside the model.
    LowMemory { free_mb: u64 },
    /// Another page is being rendered; one runs at a time.
    Busy,
    /// The browser ran and gave nothing usable, or took too long.
    Failed(String),
}

impl NotRendered {
    /// What the model is told, so it can tell the person plainly.
    pub fn words(&self) -> String {
        match self {
            Self::Off => "This page only shows its content in a browser, and reading pages \
                          with a browser is turned off in Settings."
                .to_string(),
            Self::NoBrowser => "This page only shows its content in a browser, and this pond \
                                has no browser yet. One can be downloaded in Settings, under \
                                Web."
                .to_string(),
            Self::LowMemory { free_mb } => format!(
                "This page only shows its content in a browser, and the pond has too little \
                 memory free to start one right now ({free_mb} MB of the {MIN_FREE_MB} MB it \
                 needs). It may work later."
            ),
            Self::Busy => "This page only shows its content in a browser, and the pond is \
                           reading another such page. Try again in a moment."
                .to_string(),
            Self::Failed(_) => "This page only shows its content in a browser, and the \
                                browser could not read it."
                .to_string(),
        }
    }

    /// A short word for the trace.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::NoBrowser => "no_browser",
            Self::LowMemory { .. } => "low_memory",
            Self::Busy => "busy",
            Self::Failed(_) => "failed",
        }
    }
}

/// Marks of a page that scripts fill in after it loads.
const SHELL_MARKS: [&str; 10] = [
    "id=\"root\"",
    "id=\"app\"",
    "id=\"__next\"",
    "id=\"__nuxt\"",
    "data-reactroot",
    "ng-version",
    "enable javascript",
    "requires javascript",
    "javascript is required",
    "javascript is disabled",
];

/// Roughly how many characters of `html` a person would read: what is outside tags, scripts
/// and styles. Cheap on purpose; it only decides whether a page is worth a browser.
pub fn rough_text_chars(html: &str) -> usize {
    let lower = html.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &lower[i..];
            let skip_to = ["script", "style", "noscript"].iter().find_map(|tag| {
                rest.strip_prefix('<')
                    .filter(|r| r.starts_with(tag))
                    .and_then(|_| rest.find(&format!("</{tag}")))
            });
            if let Some(end) = skip_to {
                i += end;
            }
            match lower[i..].find('>') {
                Some(close) => i += close + 1,
                None => break,
            }
        } else {
            if !bytes[i].is_ascii_whitespace() {
                count += 1;
            }
            i += 1;
        }
    }
    count
}

/// Whether `html`, which read as `readable_chars` of text, is a shell a browser would fill:
/// almost nothing to read, scripts to run, and a mark that the scripts build the page.
pub fn needs_browser(html: &str, readable_chars: usize) -> bool {
    if readable_chars >= SHELL_TEXT_CHARS {
        return false;
    }
    let lower = html.to_lowercase();
    lower.contains("<script") && SHELL_MARKS.iter().any(|mark| lower.contains(mark))
}

/// Whether the board has room to start a browser. `None` free memory is a machine that does
/// not say (a desktop), which is taken to have room.
pub fn room_to_render(free_mb: Option<u64>) -> Result<(), NotRendered> {
    match free_mb {
        Some(free_mb) if free_mb < MIN_FREE_MB => Err(NotRendered::LowMemory { free_mb }),
        _ => Ok(()),
    }
}

/// The browser's arguments for one read of `address` into its standard output. Pictures and
/// sound are off, nothing is kept between runs, and it reaches no account of anyone's.
pub fn render_args(address: &str, profile_dir: &Path) -> Vec<String> {
    vec![
        "--headless".to_string(),
        "--dump-dom".to_string(),
        format!("--virtual-time-budget={SCRIPT_BUDGET_MS}"),
        "--disable-gpu".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-extensions".to_string(),
        "--disable-background-networking".to_string(),
        "--disable-sync".to_string(),
        "--disable-default-apps".to_string(),
        "--disable-component-update".to_string(),
        "--mute-audio".to_string(),
        "--hide-scrollbars".to_string(),
        "--incognito".to_string(),
        "--blink-settings=imagesEnabled=false".to_string(),
        format!("--user-data-dir={}", profile_dir.display()),
        address.to_string(),
    ]
}

/// Program names a browser goes by on a search path, the smallest first.
pub const BROWSER_NAMES: [&str; 6] = [
    "chrome-headless-shell",
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
    "microsoft-edge",
];

/// Where a browser lives when it is installed as an application rather than on a search path.
pub fn application_paths() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ]
        .iter()
        .map(PathBuf::from)
        .collect()
    } else {
        Vec::new()
    }
}

/// The folder under the data directory the pond's own browser is downloaded into.
pub const OWN_BROWSER_DIR: &str = "browser";
/// The program inside that folder.
pub const OWN_BROWSER_NAME: &str = "chrome-headless-shell";

/// The build of the headless shell for this machine, as its publisher names platforms; `None`
/// where none is published.
pub fn download_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => Some("linux-arm64"),
        ("linux", "x86_64") => Some("linux64"),
        ("macos", "aarch64") => Some("mac-arm64"),
        ("macos", "x86_64") => Some("mac-x64"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_is_a_page_with_scripts_a_mount_point_and_nothing_to_read() {
        let shell = "<html><body><div id=\"root\"></div><script src=\"/app.js\"></script>\
                     </body></html>";
        assert!(needs_browser(shell, 0));
        assert!(needs_browser(
            "<body><noscript>You need to enable JavaScript to run this app.</noscript>\
             <script>boot()</script></body>",
            48
        ));
        // A page with words to read is read as it is, whatever it is built with.
        assert!(!needs_browser(shell, SHELL_TEXT_CHARS));
        // A short page with no scripts is just a short page.
        assert!(!needs_browser(
            "<html><body><p>Closed today.</p></body></html>",
            13
        ));
        // Scripts without a mark that they build the page: analytics on a short page.
        assert!(!needs_browser(
            "<html><body><p>Closed today.</p><script>track()</script></body></html>",
            13
        ));
    }

    #[test]
    fn rough_text_leaves_out_tags_scripts_and_styles() {
        assert_eq!(rough_text_chars("<p>Hi there</p>"), 7);
        assert_eq!(
            rough_text_chars(
                "<html><head><style>p { color: red }</style><script>var a = 'long text';\
                 </script></head><body><div id=\"root\"></div><noscript>Enable JavaScript\
                 </noscript></body></html>"
            ),
            0
        );
        assert_eq!(rough_text_chars("no tags at all"), 11);
        assert_eq!(rough_text_chars("<p>cut off <b"), 6);
    }

    #[test]
    fn a_browser_starts_only_with_room_beside_the_model() {
        assert_eq!(room_to_render(None), Ok(()));
        assert_eq!(room_to_render(Some(MIN_FREE_MB)), Ok(()));
        assert_eq!(
            room_to_render(Some(498)),
            Err(NotRendered::LowMemory { free_mb: 498 })
        );
    }

    #[test]
    fn one_read_keeps_nothing_and_loads_no_pictures() {
        let args = render_args("https://example.com/app", Path::new("/tmp/p1"));
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://example.com/app")
        );
        for must in [
            "--headless",
            "--dump-dom",
            "--incognito",
            "--blink-settings=imagesEnabled=false",
            "--user-data-dir=/tmp/p1",
            "--disable-background-networking",
        ] {
            assert!(args.iter().any(|a| a == must), "{must}");
        }
        assert!(
            !args.iter().any(|a| a.contains("no-sandbox")),
            "the browser's own sandbox stays on"
        );
    }

    #[test]
    fn every_refusal_says_what_the_person_can_do_or_expect() {
        for refusal in [
            NotRendered::Off,
            NotRendered::NoBrowser,
            NotRendered::LowMemory { free_mb: 400 },
            NotRendered::Busy,
            NotRendered::Failed("exit 1".into()),
        ] {
            let words = refusal.words();
            assert!(words.starts_with("This page only shows its content in a browser"));
            assert!(!refusal.reason().is_empty());
        }
        assert!(NotRendered::NoBrowser.words().contains("Settings"));
        assert!(NotRendered::LowMemory { free_mb: 400 }
            .words()
            .contains("400 MB"));
    }

    #[test]
    fn the_switch_is_on_until_the_household_turns_it_off() {
        assert!(is_enabled());
        set_enabled(false);
        assert!(!is_enabled());
        set_enabled(true);
    }
}
