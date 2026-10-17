//! `pond-server setup-code`, the printable card, and the setup record's startup rule. The CLI's
//! own output and the card file are the only places the setup code is ever shown (PAI-2).

use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use pond_core::network::domain::setup_card::SetupCard;
use pond_core::network::domain::setup_record::SetupRecord;
use pond_core::network::ports::setup_code_store::SetupCodeStore;

/// What `setup-code` was asked to do.
#[derive(Debug, Default, Clone)]
pub struct SetupCodeRequest {
    /// Machine-readable output on stdout instead of the card text.
    pub json: bool,
    /// Also write the printable SVG card here; `-` is stdout.
    pub card: Option<String>,
    /// Report `unclaimed`, `claimed` or `missing` and nothing else; never creates a record.
    pub status: bool,
    /// Clear the claim, so the same printed card is accepted again.
    pub rearm: bool,
}

/// The one-word claim state `--status` prints, for `giap.sh doctor`.
pub async fn status_word(store: &dyn SetupCodeStore) -> Result<&'static str> {
    Ok(match store.load().await? {
        None => "missing",
        Some(record) if record.claim_state().is_claimed() => "claimed",
        Some(_) => "unclaimed",
    })
}

/// At startup: the existing record, or a new one. A pond that already has paired devices when
/// its record is made is created claimed, so an existing pond does not suddenly accept a card.
/// `paired` is only consulted when a record must be made; if it cannot be told, none is made
/// this boot and the card is refused until the next.
pub async fn ensure_record_at_startup(
    store: &dyn SetupCodeStore,
    paired: Result<bool>,
) -> Result<SetupRecord> {
    if let Some(existing) = store.load().await? {
        return Ok(existing);
    }
    let paired = paired
        .context("could not tell whether devices are already paired, so no setup code was made")?;
    let created = store.create_if_missing().await?;
    if paired && created.claimed_at.is_none() {
        return store.mark_claimed(Utc::now(), None).await;
    }
    Ok(created)
}

/// `{"code","display","id","claimed_at","url"}`.
pub fn json_view(record: &SetupRecord) -> serde_json::Value {
    let card = record.card();
    serde_json::json!({
        "code": card.code.digits(),
        "display": card.code.display(),
        "id": card.discriminator.to_string(),
        "claimed_at": record.claimed_at.map(|at| at.to_rfc3339()),
        "url": card.qr_url(),
    })
}

/// The QR as Unicode half blocks, one string per terminal line, for a console that has no
/// other way to show a picture.
pub fn qr_terminal_lines(url: &str) -> Result<Vec<String>> {
    use qrcode::render::unicode;
    let code = qrcode::QrCode::new(url.as_bytes()).context("the link does not fit in a QR code")?;
    let image = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    Ok(image.lines().map(str::to_string).collect())
}

const CARD_WIDTH: u32 = 360;
const CARD_HEIGHT: u32 = 500;
const CARD_QR_SIZE: u32 = 260;
const CARD_LINE: &str = "Scan with Goose On The Go to set up this pond";
/// A standard QR's quiet zone on each side, which the renderer adds.
const QR_QUIET_ZONE_MODULES: u32 = 4;

/// A printable monochrome card: the QR, the code in its 4-3-4 groups, the pond's id, and one
/// line saying what to do with it. Generic sans-serif only, so it prints the same anywhere.
pub fn card_svg(card: &SetupCard) -> Result<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(card.qr_url().as_bytes())
        .context("the setup link does not fit in a QR code")?;
    // One unit per module, quiet zone included, so the scale below is exact.
    let side = code.width() as u32 + 2 * QR_QUIET_ZONE_MODULES;
    let qr = code
        .render::<svg::Color>()
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .quiet_zone(true)
        .module_dimensions(1, 1)
        .build();
    // The renderer writes a whole document; only its <svg> element nests inside the card.
    let qr = qr
        .find("<svg")
        .map(|start| &qr[start..])
        .context("the QR renderer wrote no <svg> element")?;
    let scale = f64::from(CARD_QR_SIZE) / f64::from(side);
    let qr_x = (CARD_WIDTH - CARD_QR_SIZE) / 2;
    let qr_y = 70;
    let centre = CARD_WIDTH / 2;
    Ok(format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="90mm" height="125mm" viewBox="0 0 {CARD_WIDTH} {CARD_HEIGHT}">
  <rect x="1" y="1" width="{w}" height="{h}" rx="14" fill="#ffffff" stroke="#000000" stroke-width="2"/>
  <text x="{centre}" y="46" text-anchor="middle" font-family="sans-serif" font-size="24" font-weight="bold" fill="#000000">Pond {id}</text>
  <g transform="translate({qr_x} {qr_y}) scale({scale:.4})">{qr}</g>
  <text x="{centre}" y="390" text-anchor="middle" font-family="sans-serif" font-size="13" fill="#000000">Setup code</text>
  <text x="{centre}" y="428" text-anchor="middle" font-family="sans-serif" font-size="34" font-weight="bold" letter-spacing="2" fill="#000000">{code}</text>
  <text x="{centre}" y="470" text-anchor="middle" font-family="sans-serif" font-size="14" fill="#000000">{CARD_LINE}</text>
</svg>
"##,
        w = CARD_WIDTH - 2,
        h = CARD_HEIGHT - 2,
        id = card.discriminator,
        code = card.code.display(),
    ))
}

/// Written `0600` from the first byte: the card carries the code.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    // `mode` applies only to a new file; an existing one is narrowed before anything is written.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting {} to 0600", path.display()))?;
    }
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", path.display()))
}

/// The human-readable card, as lines.
fn card_text(record: &SetupRecord) -> Vec<String> {
    let card = record.card();
    let mut lines = vec![
        format!("Setup code: {}", card.code.display()),
        format!("Pond id: {}", card.discriminator),
        match record.claimed_at {
            Some(at) => format!(
                "Claimed: {} (a phone has paired; the card is no longer accepted)",
                at.to_rfc3339()
            ),
            None => "Unclaimed: the first phone to scan this card sets up this pond".to_string(),
        },
    ];
    match qr_terminal_lines(&card.qr_url()) {
        Ok(qr) => lines.extend(qr.into_iter().map(|line| format!("  {line}"))),
        Err(error) => lines.push(format!("(no QR: {error:#})")),
    }
    lines.push(format!("URL: {}", card.qr_url()));
    lines
}

/// Runs `setup-code` against `store`, writing to `out` (stdout) and `err` (stderr).
pub async fn run(
    store: &dyn SetupCodeStore,
    request: &SetupCodeRequest,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    if request.status {
        writeln!(out, "{}", status_word(store).await?)?;
        return Ok(());
    }
    let card_to_stdout = request.card.as_deref() == Some("-");
    if card_to_stdout && request.json {
        bail!("--json and --card - both write to stdout; give --card a file path");
    }

    let record = if request.rearm {
        let record = store.rearm().await?;
        writeln!(
            err,
            "Re-armed: the printed card is accepted again until a phone pairs. Devices already \
             paired keep working; remove them under Devices if this pond is changing hands."
        )?;
        record
    } else {
        store.create_if_missing().await?
    };

    let human: &mut dyn Write = if card_to_stdout { err } else { out };
    if request.json {
        writeln!(human, "{}", json_view(&record))?;
    } else {
        for line in card_text(&record) {
            writeln!(human, "{line}")?;
        }
    }

    match request.card.as_deref() {
        None => {}
        Some("-") => out.write_all(card_svg(&record.card())?.as_bytes())?,
        Some(path) => {
            let path = Path::new(path);
            write_private(path, card_svg(&record.card())?.as_bytes())?;
            writeln!(
                human,
                "Card written to {}. Print it and keep it with the pond.",
                path.display()
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pond_core::network::mocks::mock_setup_code_store::MockSetupCodeStore;
    use pond_infra::file_setup_code_store::{FileSetupCodeStore, SETUP_DIR};

    async fn run_to_strings(
        store: &dyn SetupCodeStore,
        request: SetupCodeRequest,
    ) -> (Result<()>, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let result = run(store, &request, &mut out, &mut err).await;
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[tokio::test]
    async fn status_never_creates_a_record_or_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSetupCodeStore::new(dir.path());
        let request = SetupCodeRequest {
            status: true,
            ..Default::default()
        };
        let (result, out, _) = run_to_strings(&store, request.clone()).await;
        result.unwrap();
        assert_eq!(out, "missing\n");
        assert!(!dir.path().join(SETUP_DIR).exists());

        store.create_if_missing().await.unwrap();
        let (_, out, _) = run_to_strings(&store, request.clone()).await;
        assert_eq!(out, "unclaimed\n");
        store.mark_claimed(Utc::now(), None).await.unwrap();
        let (_, out, _) = run_to_strings(&store, request).await;
        assert_eq!(out, "claimed\n");
    }

    #[tokio::test]
    async fn status_reports_an_unreadable_store_as_an_error_and_never_the_code() {
        let store = MockSetupCodeStore::unclaimed();
        store.set_failing(true);
        let (result, out, err) = run_to_strings(
            &store,
            SetupCodeRequest {
                status: true,
                ..Default::default()
            },
        )
        .await;
        assert!(result.is_err());
        assert!(out.is_empty() && err.is_empty());
    }

    #[tokio::test]
    async fn the_default_output_is_the_card_text_and_it_creates_the_record() {
        let store = MockSetupCodeStore::new();
        let (result, out, err) = run_to_strings(&store, SetupCodeRequest::default()).await;
        result.unwrap();
        assert!(err.is_empty());
        assert!(out.contains("Setup code: 7992-739-8713"), "{out}");
        assert!(out.contains("Pond id: 7f3a"));
        assert!(out.contains("Unclaimed"));
        assert!(out.contains("URL: pond://setup?v=1&code=79927398713&id=7f3a"));
        assert!(store.record().is_some());
    }

    #[tokio::test]
    async fn json_has_exactly_the_documented_fields() {
        let store = MockSetupCodeStore::unclaimed();
        let at = Utc.with_ymd_and_hms(2026, 10, 17, 9, 0, 0).unwrap();
        store.set_claimed_at(Some(at));
        let (result, out, _) = run_to_strings(
            &store,
            SetupCodeRequest {
                json: true,
                ..Default::default()
            },
        )
        .await;
        result.unwrap();
        let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "code": "79927398713",
                "display": "7992-739-8713",
                "id": "7f3a",
                "claimed_at": "2026-10-17T09:00:00+00:00",
                "url": "pond://setup?v=1&code=79927398713&id=7f3a",
            })
        );
    }

    #[tokio::test]
    async fn a_card_to_stdout_sends_the_text_to_stderr() {
        let store = MockSetupCodeStore::unclaimed();
        let (result, out, err) = run_to_strings(
            &store,
            SetupCodeRequest {
                card: Some("-".into()),
                ..Default::default()
            },
        )
        .await;
        result.unwrap();
        assert!(out.starts_with("<?xml"), "{out}");
        assert!(out.trim_end().ends_with("</svg>"));
        assert!(err.contains("Setup code: 7992-739-8713"));

        let (result, _, _) = run_to_strings(
            &store,
            SetupCodeRequest {
                card: Some("-".into()),
                json: true,
                ..Default::default()
            },
        )
        .await;
        assert!(result.is_err(), "two documents cannot share stdout");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_card_file_is_written_0600_even_over_a_wider_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card.svg");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let store = MockSetupCodeStore::unclaimed();
        let (result, out, _) = run_to_strings(
            &store,
            SetupCodeRequest {
                card: Some(path.display().to_string()),
                ..Default::default()
            },
        )
        .await;
        result.unwrap();
        assert!(out.contains("Print it and keep it with the pond"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("7992-739-8713"));
    }

    #[tokio::test]
    async fn rearm_clears_the_claim_and_keeps_the_code() {
        let store = MockSetupCodeStore::unclaimed();
        store.set_claimed_at(Some(Utc::now()));
        let (result, out, err) = run_to_strings(
            &store,
            SetupCodeRequest {
                rearm: true,
                ..Default::default()
            },
        )
        .await;
        result.unwrap();
        assert!(err.contains("Devices already paired keep working"));
        assert!(out.contains("Setup code: 7992-739-8713"));
        assert_eq!(store.record().unwrap().claimed_at, None);
    }

    #[test]
    fn the_card_is_monochrome_sans_serif_and_carries_the_code_the_id_and_the_line() {
        let card = MockSetupCodeStore::fixed_record().card();
        let svg = card_svg(&card).unwrap();
        assert!(svg.contains("7992-739-8713"));
        assert!(svg.contains("Pond 7f3a"));
        assert!(svg.contains(CARD_LINE));
        assert_eq!(
            svg.matches("<?xml").count(),
            1,
            "one document, the QR nested"
        );
        assert!(
            !svg.contains("@import") && !svg.contains("url("),
            "no external fonts"
        );
        for colour in svg.match_indices('#').map(|(i, _)| &svg[i..i + 7]) {
            assert!(
                colour == "#000000" || colour == "#ffffff",
                "only black and white: {colour}"
            );
        }
        for family in svg
            .match_indices("font-family=\"")
            .map(|(i, _)| &svg[i + 13..])
        {
            assert!(family.starts_with("sans-serif\""), "{family:.20}");
        }
    }

    #[tokio::test]
    async fn a_new_record_on_a_pond_with_paired_devices_is_made_claimed() {
        let store = MockSetupCodeStore::new();
        let record = ensure_record_at_startup(&store, Ok(true)).await.unwrap();
        assert!(record.claimed_at.is_some());

        let fresh = MockSetupCodeStore::new();
        let record = ensure_record_at_startup(&fresh, Ok(false)).await.unwrap();
        assert_eq!(record.claimed_at, None);
    }

    #[tokio::test]
    async fn an_existing_record_is_never_claimed_by_startup() {
        let store = MockSetupCodeStore::unclaimed();
        let record = ensure_record_at_startup(&store, Ok(true)).await.unwrap();
        assert_eq!(
            record.claimed_at, None,
            "a pond re-armed by its owner must stay re-armed across a restart"
        );
        assert!(
            ensure_record_at_startup(&store, Err(anyhow::anyhow!("db down")))
                .await
                .is_ok(),
            "an existing record needs no device count"
        );
    }

    #[tokio::test]
    async fn no_record_is_made_when_paired_devices_cannot_be_counted() {
        let store = MockSetupCodeStore::new();
        assert!(
            ensure_record_at_startup(&store, Err(anyhow::anyhow!("db down")))
                .await
                .is_err()
        );
        assert_eq!(store.record(), None);
    }
}
