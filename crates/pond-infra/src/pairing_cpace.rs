//! CPace for phone pairing: the `CPACE-RISTR255-SHA512` suite of `draft-irtf-cfrg-cpace-21`.
//!
//! The six-digit pairing code is the password-related string (PRS). Nothing a passive or active
//! party sees in a run can be tested against a code offline: each guess costs one live exchange
//! with the Pond, which [`crate::sqlite_handshake`] limits per code.
//!
//! Roles and inputs (design note `pake-design-508.md`, 2026-10-13):
//!
//! - The Pond is initiator **A**, the phone responder **B**, so the protocol fits two HTTP round
//!   trips and the phone confirms first.
//! - `CI = lv_cat("goose-pair-cpace-v1", spki_pin, client_id)`, where `spki_pin` is the ASCII
//!   `sha256/<base64>` pin. The Pond always uses its own pin and the phone the pin it pinned to,
//!   so a TLS interceptor with its own key leaves the two sides with different generators.
//! - `sid = challenge (32 bytes, Pond) || client_nonce (32 bytes, phone)` (draft section 10.9).
//! - `ADa = ADb = b""`: everything known up front is already in CI (draft section 10.1.1).
//! - Key confirmation (draft section 10.4): `mac_key = SHA-512("CPaceMac" || sid || ISK)`, and
//!   each side's tag is `HMAC-SHA-512(mac_key, lv_cat(label, Ya, ADa, Yb, ADb))` with the labels
//!   `goose-pair-cpace-client-v1` and `goose-pair-cpace-server-v1`. The labels differ, so a tag
//!   cannot be reflected; the Pond reveals its tag only after the phone's has verified.
//!
//! The group is ristretto255 from `curve25519-dalek`; this module is only the draft's glue
//! (`lv_cat`, the generator, ISK, the tags). It is held to the draft's Appendix B.3 vectors and
//! to the cross-language exchange vectors in `tests/fixtures/cpace-vectors.json`.
//!
//! A run over the BLE setup channel also derives that channel's two keys from ISK
//! (`docs/architecture/pond-setup-channel.md`, "Sealing"). ISK leaves this module as nothing else.
//! A paired phone resuming there uses its refresh token's hash as the password, under its own label.
//!
//! The Pond side ([`Initiator`]) is crate-private. The phone side ([`Responder`]) exists only for
//! tests and the `cpace-client` feature, which the integration tests use to pair like a phone.

use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::IsIdentity;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use pond_core::security::ports::handshake::{ChannelKeys, CHANNEL_KEY_LEN};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha512};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

/// The suite name the Pond announces in its challenge.
pub const SUITE: &str = "CPACE-RISTR255-SHA512";

/// `G_Ristretto255.DSI`.
const DSI: &[u8] = b"CPaceRistretto255";
/// `G.DSI || b"_ISK"`.
const DSI_ISK: &[u8] = b"CPaceRistretto255_ISK";
/// SHA-512's input block size, `H.s_in_bytes`.
const S_IN_BYTES: usize = 128;
/// Prefix of the key-confirmation MAC key (draft section 10.4).
const MAC_KEY_PREFIX: &[u8] = b"CPaceMac";
/// First field of the channel identifier; a breaking draft revision means a new label.
const CHANNEL_LABEL: &[u8] = b"goose-pair-cpace-v1";
/// The setup code's own label, so a setup run and a pairing run never share a generator.
const SETUP_CHANNEL_LABEL: &[u8] = b"goose-setup-cpace-v1";
/// A paired device's own label, so its refresh hash never shares a generator with a code.
const DEVICE_CHANNEL_LABEL: &[u8] = b"goose-device-cpace-v1";
/// Label of the phone's confirmation tag.
const CLIENT_TAG_LABEL: &[u8] = b"goose-pair-cpace-client-v1";
/// Label of the Pond's confirmation tag.
const SERVER_TAG_LABEL: &[u8] = b"goose-pair-cpace-server-v1";
/// `ADa` and `ADb`.
const AD: &[u8] = b"";
/// HKDF info for `k_p2c`, the key of what the Pond sends over the setup channel.
pub const CHANNEL_POND_TO_PHONE_INFO: &str = "goose-setup-channel-v1 pond to phone";
/// HKDF info for `k_c2p`, the key of what the phone sends.
pub const CHANNEL_PHONE_TO_POND_INFO: &str = "goose-setup-channel-v1 phone to pond";
/// Length of ISK and of the HKDF-SHA-512 pseudorandom key.
const ISK_LEN: usize = 64;

/// Length of an encoded ristretto255 element (a public share).
pub const SHARE_LEN: usize = 32;
/// Length of a confirmation tag (HMAC-SHA-512).
pub const TAG_LEN: usize = 64;
/// Length of each half of the session identifier.
pub const SID_PART_LEN: usize = 32;

/// Why a run was aborted. Each is a failed attempt; neither says anything about the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpaceError {
    /// The peer's share did not decode, or the shared point was the identity.
    InvalidShare,
    /// The peer's confirmation tag was wrong.
    InvalidConfirmation,
}

/// `prepend_len` (draft A.1.1): the LEB128 length of `data`, then `data`.
fn prepend_len(out: &mut Vec<u8>, data: &[u8]) {
    let mut length = data.len();
    loop {
        if length < 128 {
            out.push(length as u8);
        } else {
            out.push((length & 0x7f) as u8 | 0x80);
        }
        length >>= 7;
        if length == 0 {
            break;
        }
    }
    out.extend_from_slice(data);
}

/// `lv_cat` (draft A.1.3): each part length-prefixed, concatenated.
fn lv_cat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        prepend_len(&mut out, part);
    }
    out
}

/// The channel identifier: this protocol's label, the TLS key pin, and the phone's id.
pub fn channel_identifier(spki_pin: &str, client_id: &str) -> Vec<u8> {
    lv_cat(&[CHANNEL_LABEL, spki_pin.as_bytes(), client_id.as_bytes()])
}

/// As [`channel_identifier`], for a run whose password is the printed setup code.
pub fn setup_channel_identifier(spki_pin: &str, client_id: &str) -> Vec<u8> {
    lv_cat(&[
        SETUP_CHANNEL_LABEL,
        spki_pin.as_bytes(),
        client_id.as_bytes(),
    ])
}

/// As [`channel_identifier`], for a paired device resuming with its refresh token's hash.
pub fn device_channel_identifier(spki_pin: &str, client_id: &str) -> Vec<u8> {
    lv_cat(&[
        DEVICE_CHANNEL_LABEL,
        spki_pin.as_bytes(),
        client_id.as_bytes(),
    ])
}

/// A device run's password as the phone derives it: the lowercase hex SHA-256 of its current
/// refresh token, the text the pond stores as `session_tokens.refresh_hash`.
#[cfg(any(test, feature = "cpace-client"))]
pub fn device_password(refresh_token: &str) -> Zeroizing<String> {
    use std::fmt::Write as _;
    let digest = sha2::Sha256::digest(refresh_token.as_bytes());
    let mut password = Zeroizing::new(String::with_capacity(64));
    for byte in digest {
        let _ = write!(password, "{byte:02x}");
    }
    password
}

/// The session identifier: the Pond's challenge, then the phone's nonce.
pub fn session_id(challenge: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    [challenge, client_nonce].concat()
}

/// `generator_string` (draft A.2), zero-padded so DSI and PRS fill the first hash block.
fn generator_string(prs: &[u8], ci: &[u8], sid: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut prefixed_prs = Zeroizing::new(Vec::new());
    prepend_len(&mut prefixed_prs, prs);
    let mut prefixed_dsi = Vec::new();
    prepend_len(&mut prefixed_dsi, DSI);
    let len_zpad = S_IN_BYTES.saturating_sub(1 + prefixed_prs.len() + prefixed_dsi.len());
    Zeroizing::new(lv_cat(&[DSI, prs, &vec![0u8; len_zpad], ci, sid]))
}

/// `G.calculate_generator`: SHA-512 of the generator string, through RFC 9496's element
/// derivation (section 4.3.4). Deliberately not RFC 9380's `hash_to_ristretto255`.
fn generator(prs: &[u8], ci: &[u8], sid: &[u8]) -> RistrettoPoint {
    let mut hash = Zeroizing::new([0u8; 64]);
    hash.copy_from_slice(&Sha512::digest(generator_string(prs, ci, sid).as_slice()));
    RistrettoPoint::from_uniform_bytes(&hash)
}

/// `G.sample_scalar` (draft 8.3): 32 random bytes with bits 252 to 255 cleared, read
/// little-endian. Always below the group order, so canonical; zero is rejected.
fn scalar_from_bytes(mut bytes: [u8; 32]) -> Option<Scalar> {
    bytes[31] &= 0x0f;
    let scalar = Option::<Scalar>::from(Scalar::from_canonical_bytes(bytes));
    bytes.zeroize();
    scalar.filter(|s| *s != Scalar::ZERO)
}

fn sample_scalar() -> Scalar {
    loop {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        if let Some(scalar) = scalar_from_bytes(bytes) {
            return scalar;
        }
    }
}

/// `G.scalar_mult_vfy` plus the abort on `G.I`: `None` when `share` does not decode
/// (non-canonical bytes included) or the product is the identity.
fn scalar_mult_vfy(scalar: &Scalar, share: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
    let point = CompressedRistretto::from_slice(share).ok()?.decompress()?;
    let product = scalar * point;
    if product.is_identity() {
        return None;
    }
    Some(Zeroizing::new(product.compress().to_bytes()))
}

/// `ISK = SHA-512(lv_cat(DSI_ISK, sid, K) || transcript_ir(Ya, ADa, Yb, ADb))`.
fn intermediate_session_key(
    sid: &[u8],
    k: &[u8],
    ya: &[u8],
    yb: &[u8],
) -> Zeroizing<[u8; ISK_LEN]> {
    let mut hasher = Sha512::new();
    hasher.update(Zeroizing::new(lv_cat(&[DSI_ISK, sid, k])).as_slice());
    hasher.update(lv_cat(&[ya, AD]));
    hasher.update(lv_cat(&[yb, AD]));
    let mut isk = Zeroizing::new([0u8; ISK_LEN]);
    isk.copy_from_slice(&hasher.finalize());
    isk
}

/// `prk = HKDF-Extract(SHA-512, salt = sid, ikm = ISK)`, then one HKDF-Expand per direction.
fn channel_key_schedule(isk: &[u8], sid: &[u8]) -> (Zeroizing<[u8; ISK_LEN]>, ChannelKeys) {
    let (mut extracted, hkdf) = Hkdf::<Sha512>::extract(Some(sid), isk);
    let mut prk = Zeroizing::new([0u8; ISK_LEN]);
    prk.copy_from_slice(&extracted);
    extracted.as_mut_slice().zeroize();
    let mut pond_to_phone = Zeroizing::new([0u8; CHANNEL_KEY_LEN]);
    let mut phone_to_pond = Zeroizing::new([0u8; CHANNEL_KEY_LEN]);
    hkdf.expand(
        CHANNEL_POND_TO_PHONE_INFO.as_bytes(),
        pond_to_phone.as_mut_slice(),
    )
    .expect("32 bytes is far inside HKDF-SHA-512's output limit");
    hkdf.expand(
        CHANNEL_PHONE_TO_POND_INFO.as_bytes(),
        phone_to_pond.as_mut_slice(),
    )
    .expect("32 bytes is far inside HKDF-SHA-512's output limit");
    (prk, ChannelKeys::new(&pond_to_phone, &phone_to_pond))
}

/// The keys both sides derive once they hold `K`.
struct Confirmation {
    mac_key: Zeroizing<[u8; 64]>,
    isk: Zeroizing<[u8; ISK_LEN]>,
    sid: Vec<u8>,
    ya: [u8; SHARE_LEN],
    yb: [u8; SHARE_LEN],
}

impl Confirmation {
    fn derive(sid: &[u8], k: &[u8], ya: [u8; SHARE_LEN], yb: [u8; SHARE_LEN]) -> Self {
        let isk = intermediate_session_key(sid, k, &ya, &yb);
        let mut hasher = Sha512::new();
        hasher.update(MAC_KEY_PREFIX);
        hasher.update(sid);
        hasher.update(isk.as_slice());
        let mut mac_key = Zeroizing::new([0u8; 64]);
        mac_key.copy_from_slice(&hasher.finalize());
        Self {
            mac_key,
            isk,
            sid: sid.to_vec(),
            ya,
            yb,
        }
    }

    fn channel_keys(&self) -> ChannelKeys {
        channel_key_schedule(self.isk.as_slice(), &self.sid).1
    }

    fn tag(&self, label: &[u8]) -> [u8; TAG_LEN] {
        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(self.mac_key.as_slice())
            .expect("HMAC takes a key of any length");
        mac.update(&lv_cat(&[label, &self.ya, AD, &self.yb, AD]));
        mac.finalize().into_bytes().into()
    }

    fn client_tag(&self) -> [u8; TAG_LEN] {
        self.tag(CLIENT_TAG_LABEL)
    }

    fn server_tag(&self) -> [u8; TAG_LEN] {
        self.tag(SERVER_TAG_LABEL)
    }
}

/// The Pond's half of one run, held between `init` and `verify`. Its scalar never leaves
/// memory and is zeroized when the run is finished or dropped.
pub(crate) struct Initiator {
    scalar: Scalar,
    share: [u8; SHARE_LEN],
    sid: Vec<u8>,
}

impl Initiator {
    /// Commit to `prs` under `ci` and `sid`, and produce `Ya`.
    pub(crate) fn start(prs: &[u8], ci: &[u8], sid: Vec<u8>) -> Self {
        Self::with_scalar(sample_scalar(), prs, ci, sid)
    }

    fn with_scalar(scalar: Scalar, prs: &[u8], ci: &[u8], sid: Vec<u8>) -> Self {
        let share = (scalar * generator(prs, ci, &sid)).compress().to_bytes();
        Self { scalar, share, sid }
    }

    /// `Ya`, for the challenge.
    pub(crate) fn share(&self) -> [u8; SHARE_LEN] {
        self.share
    }

    /// Check the phone's `Yb` and `T_client`; on success, the Pond's `T_server` and the run.
    pub(crate) fn finish(self, yb: &[u8], client_tag: &[u8]) -> Result<Confirmed, CpaceError> {
        let k = scalar_mult_vfy(&self.scalar, yb).ok_or(CpaceError::InvalidShare)?;
        let yb: [u8; SHARE_LEN] = yb.try_into().map_err(|_| CpaceError::InvalidShare)?;
        let confirmation = Confirmation::derive(&self.sid, k.as_slice(), self.share, yb);
        if !bool::from(confirmation.client_tag().as_slice().ct_eq(client_tag)) {
            return Err(CpaceError::InvalidConfirmation);
        }
        Ok(Confirmed {
            server_tag: confirmation.server_tag(),
            confirmation,
        })
    }
}

/// A run the phone confirmed. Its ISK stays inside, wiped on drop, and comes out only as
/// [`Confirmed::channel_keys`].
pub(crate) struct Confirmed {
    /// `T_server`, the Pond's proof, sent once the tokens exist.
    pub(crate) server_tag: [u8; TAG_LEN],
    confirmation: Confirmation,
}

impl Confirmed {
    /// The setup channel's keys for this run. Called for a channel run only.
    pub(crate) fn channel_keys(&self) -> ChannelKeys {
        self.confirmation.channel_keys()
    }
}

impl Drop for Initiator {
    fn drop(&mut self) {
        self.scalar.zeroize();
    }
}

/// The phone's half of a run: what a pairing client computes from the code, its pin, its id,
/// the challenge and its own nonce. Tests and the `cpace-client` feature only.
#[cfg(any(test, feature = "cpace-client"))]
pub struct Responder {
    share: [u8; SHARE_LEN],
    confirmation: Confirmation,
}

#[cfg(any(test, feature = "cpace-client"))]
impl Responder {
    /// Answer the Pond's `Ya`: derive `Yb`, `K`, ISK and both tags.
    pub fn respond(prs: &[u8], ci: &[u8], sid: &[u8], ya: &[u8]) -> Result<Self, CpaceError> {
        Self::with_scalar(sample_scalar(), prs, ci, sid, ya)
    }

    fn with_scalar(
        mut scalar: Scalar,
        prs: &[u8],
        ci: &[u8],
        sid: &[u8],
        ya: &[u8],
    ) -> Result<Self, CpaceError> {
        let share = (scalar * generator(prs, ci, sid)).compress().to_bytes();
        let k = scalar_mult_vfy(&scalar, ya);
        scalar.zeroize();
        let k = k.ok_or(CpaceError::InvalidShare)?;
        let ya: [u8; SHARE_LEN] = ya.try_into().map_err(|_| CpaceError::InvalidShare)?;
        Ok(Self {
            share,
            confirmation: Confirmation::derive(sid, k.as_slice(), ya, share),
        })
    }

    /// `Yb`, sent in `verify`.
    pub fn share(&self) -> [u8; SHARE_LEN] {
        self.share
    }

    /// `T_client`, sent in `verify`.
    pub fn confirmation(&self) -> [u8; TAG_LEN] {
        self.confirmation.client_tag()
    }

    /// Check the Pond's `T_server`; a client that gets `false` must discard the tokens.
    pub fn verify_server(&self, server_tag: &[u8]) -> bool {
        self.confirmation
            .server_tag()
            .as_slice()
            .ct_eq(server_tag)
            .into()
    }

    /// The setup channel's keys, as the phone derives them from its own ISK.
    pub fn channel_keys(&self) -> ChannelKeys {
        self.confirmation.channel_keys()
    }
}

/// Every value of the setup channel's key schedule for a given ISK, for the vector file.
#[cfg(any(test, feature = "cpace-client"))]
pub struct ChannelKeySchedule {
    pub prk: [u8; ISK_LEN],
    pub keys: ChannelKeys,
}

/// The key schedule over a caller's ISK and sid; a Pond's own ISK never reaches it.
#[cfg(any(test, feature = "cpace-client"))]
pub fn channel_key_schedule_for(isk: &[u8], sid: &[u8]) -> ChannelKeySchedule {
    let (prk, keys) = channel_key_schedule(isk, sid);
    ChannelKeySchedule { prk: *prk, keys }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn load(name: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
    }

    fn field(v: &Value, path: &[&str]) -> Vec<u8> {
        let mut v = v;
        for key in path {
            v = &v[*key];
        }
        unhex(
            v.as_str()
                .unwrap_or_else(|| panic!("{path:?} is not a string")),
        )
    }

    fn scalar(bytes: &[u8]) -> Scalar {
        scalar_from_bytes(bytes.try_into().unwrap()).unwrap()
    }

    fn finish_tag(pond: Initiator, yb: &[u8], tag: &[u8]) -> Result<[u8; TAG_LEN], CpaceError> {
        pond.finish(yb, tag).map(|confirmed| confirmed.server_tag)
    }

    // ---- The draft's own vectors (Appendix A.1 and B.3) ----------------------------------

    #[test]
    fn string_utilities_match_the_draft() {
        let v = load("cpace-ristr255-sha512-draft21.json");
        let mut out = Vec::new();
        prepend_len(&mut out, b"");
        assert_eq!(out, field(&v, &["prepend_len", "empty"]));
        let mut out = Vec::new();
        prepend_len(&mut out, b"1234");
        assert_eq!(out, field(&v, &["prepend_len", "1234"]));
        let mut out = Vec::new();
        prepend_len(&mut out, &(0u8..127).collect::<Vec<_>>());
        assert_eq!(out, field(&v, &["prepend_len", "range_127"]));
        let mut out = Vec::new();
        prepend_len(&mut out, &(0u8..128).collect::<Vec<_>>());
        assert_eq!(out, field(&v, &["prepend_len", "range_128"]));
        assert_eq!(
            lv_cat(&[b"1234", b"5", b"", b"678"]),
            field(&v, &["lv_cat", "1234_5_empty_678"])
        );
    }

    #[test]
    fn a_full_run_matches_the_draft_vectors() {
        let v = load("cpace-ristr255-sha512-draft21.json");
        let prs = field(&v, &["inputs", "PRS"]);
        let ci = field(&v, &["inputs", "CI"]);
        let sid = field(&v, &["inputs", "sid"]);

        assert_eq!(
            generator_string(&prs, &ci, &sid).as_slice(),
            field(&v, &["generator", "generator_string"]).as_slice()
        );
        assert_eq!(
            Sha512::digest(generator_string(&prs, &ci, &sid).as_slice()).as_slice(),
            field(&v, &["generator", "hash"]).as_slice()
        );
        assert_eq!(
            generator(&prs, &ci, &sid).compress().to_bytes().as_slice(),
            field(&v, &["generator", "g"]).as_slice()
        );

        // The draft's ADa and ADb are not empty, so this checks the group and ISK math with
        // the draft's inputs, through the same functions the Pond uses.
        let ya = scalar(&field(&v, &["inputs", "ya"]));
        let yb = scalar(&field(&v, &["inputs", "yb"]));
        let g = generator(&prs, &ci, &sid);
        let ya_share = (ya * g).compress().to_bytes();
        let yb_share = (yb * g).compress().to_bytes();
        assert_eq!(
            ya_share.as_slice(),
            field(&v, &["outputs", "Ya"]).as_slice()
        );
        assert_eq!(
            yb_share.as_slice(),
            field(&v, &["outputs", "Yb"]).as_slice()
        );

        let k_a = scalar_mult_vfy(&ya, &yb_share).unwrap();
        let k_b = scalar_mult_vfy(&yb, &ya_share).unwrap();
        assert_eq!(k_a.as_slice(), field(&v, &["outputs", "K"]).as_slice());
        assert_eq!(k_a.as_slice(), k_b.as_slice());

        let ada = field(&v, &["inputs", "ADa"]);
        let adb = field(&v, &["inputs", "ADb"]);
        let mut transcript = lv_cat(&[&ya_share, &ada]);
        transcript.extend(lv_cat(&[&yb_share, &adb]));
        assert_eq!(transcript, field(&v, &["outputs", "transcript_ir"]));
        let mut isk_input = lv_cat(&[DSI_ISK, &sid, k_a.as_slice()]);
        isk_input.extend(&transcript);
        assert_eq!(
            Sha512::digest(&isk_input).as_slice(),
            field(&v, &["outputs", "ISK_ir"]).as_slice()
        );
    }

    /// With empty AD, `intermediate_session_key` is the draft's ISK formula exactly.
    #[test]
    fn the_isk_function_is_the_drafts_formula_with_empty_ad() {
        let v = load("cpace-ristr255-sha512-draft21.json");
        let sid = field(&v, &["inputs", "sid"]);
        let k = field(&v, &["outputs", "K"]);
        let ya = field(&v, &["outputs", "Ya"]);
        let yb = field(&v, &["outputs", "Yb"]);
        let mut input = lv_cat(&[DSI_ISK, &sid, &k]);
        input.extend(lv_cat(&[&ya, b""]));
        input.extend(lv_cat(&[&yb, b""]));
        assert_eq!(
            intermediate_session_key(&sid, &k, &ya, &yb).as_slice(),
            Sha512::digest(&input).as_slice()
        );
    }

    #[test]
    fn scalar_mult_matches_the_draft_and_invalid_points_abort() {
        let v = load("cpace-ristr255-sha512-draft21.json");
        let s = scalar(&field(&v, &["scalar_mult", "s"]));
        let x = field(&v, &["scalar_mult", "X"]);
        assert_eq!(
            scalar_mult_vfy(&s, &x).unwrap().as_slice(),
            field(&v, &["scalar_mult", "result"]).as_slice()
        );
        for invalid in ["Y_i1", "Y_i2"] {
            assert!(
                scalar_mult_vfy(&s, &field(&v, &["invalid_points", invalid])).is_none(),
                "{invalid} must abort"
            );
        }
        assert!(
            scalar_mult_vfy(&s, &[1u8; 31]).is_none(),
            "a short share aborts"
        );
    }

    #[test]
    fn sampled_scalars_are_canonical_and_never_zero() {
        assert!(scalar_from_bytes([0u8; 32]).is_none());
        // Every bit set: masking leaves 2^252 - 1, which is below the group order.
        let max = scalar_from_bytes([0xff; 32]).unwrap();
        assert_eq!(max.to_bytes()[31], 0x0f);
        for _ in 0..64 {
            assert_ne!(sample_scalar(), Scalar::ZERO);
        }
    }

    // ---- The protocol as the Pond and a phone run it ------------------------------------

    const PIN: &str = "sha256/TnkUsN+AaLec3BDTh/KUwSokXTmCq2+4rlfBnmJxPkE=";
    const OTHER_PIN: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn sid() -> Vec<u8> {
        session_id(&[7u8; 32], &[9u8; 32])
    }

    #[test]
    fn a_full_exchange_agrees_and_each_side_checks_the_other() {
        let ci = channel_identifier(PIN, "phone-1");
        let pond = Initiator::start(b"482915", &ci, sid());
        let phone = Responder::respond(b"482915", &ci, &sid(), &pond.share()).unwrap();
        let server_tag = finish_tag(pond, &phone.share(), &phone.confirmation()).unwrap();
        assert!(phone.verify_server(&server_tag));
        assert_ne!(server_tag, phone.confirmation(), "the two tags must differ");
    }

    #[test]
    fn a_wrong_code_fails_confirmation() {
        let ci = channel_identifier(PIN, "phone-1");
        let pond = Initiator::start(b"482915", &ci, sid());
        let phone = Responder::respond(b"482916", &ci, &sid(), &pond.share()).unwrap();
        assert_eq!(
            finish_tag(pond, &phone.share(), &phone.confirmation()),
            Err(CpaceError::InvalidConfirmation)
        );
    }

    #[test]
    fn a_phone_that_pinned_another_key_fails_confirmation() {
        let pond = Initiator::start(b"482915", &channel_identifier(PIN, "phone-1"), sid());
        let ci = channel_identifier(OTHER_PIN, "phone-1");
        let phone = Responder::respond(b"482915", &ci, &sid(), &pond.share()).unwrap();
        assert_eq!(
            finish_tag(pond, &phone.share(), &phone.confirmation()),
            Err(CpaceError::InvalidConfirmation)
        );
    }

    /// What a phone with the wrong code sends is a uniformly random-looking group element and
    /// a tag under a key nobody else holds: the Pond's view of the run is the same function of
    /// fresh randomness whatever the code. The only check is the online one.
    #[test]
    fn the_pond_reveals_nothing_until_the_phone_has_confirmed() {
        let ci = channel_identifier(PIN, "phone-1");
        let pond = Initiator::start(b"482915", &ci, sid());
        let ya = pond.share();
        // `Ya` is a fresh random multiple of the generator: two runs on one code differ.
        let again = Initiator::start(b"482915", &ci, sid());
        assert_ne!(ya, again.share());
        // A failed confirmation returns no tag at all.
        let phone = Responder::respond(b"000000", &ci, &sid(), &ya).unwrap();
        assert!(finish_tag(pond, &phone.share(), &phone.confirmation()).is_err());
    }

    #[test]
    fn identity_and_malformed_shares_abort() {
        let ci = channel_identifier(PIN, "phone-1");
        for bad in [vec![0u8; 32], vec![0xffu8; 32], vec![1u8; 31]] {
            let pond = Initiator::start(b"482915", &ci, sid());
            assert_eq!(
                finish_tag(pond, &bad, &[0u8; 64]),
                Err(CpaceError::InvalidShare)
            );
            assert!(Responder::respond(b"482915", &ci, &sid(), &bad).is_err());
        }
    }

    #[test]
    fn both_sides_derive_the_same_channel_keys_and_each_direction_its_own() {
        let ci = setup_channel_identifier(PIN, "phone-1");
        let pond = Initiator::start(b"79927398713", &ci, sid());
        let phone = Responder::respond(b"79927398713", &ci, &sid(), &pond.share()).unwrap();
        let confirmed = pond.finish(&phone.share(), &phone.confirmation()).unwrap();
        let (ours, theirs) = (confirmed.channel_keys(), phone.channel_keys());
        assert_eq!(ours.pond_to_phone(), theirs.pond_to_phone());
        assert_eq!(ours.phone_to_pond(), theirs.phone_to_pond());
        assert_ne!(ours.pond_to_phone(), ours.phone_to_pond());

        let again = Initiator::start(b"79927398713", &ci, sid());
        let phone_again = Responder::respond(b"79927398713", &ci, &sid(), &again.share()).unwrap();
        assert_ne!(
            again
                .finish(&phone_again.share(), &phone_again.confirmation())
                .unwrap()
                .channel_keys()
                .pond_to_phone(),
            ours.pond_to_phone(),
            "a fresh run, with fresh scalars, has fresh keys"
        );
    }

    /// RFC 5869 with SHA-512, spelled out with HMAC so the salt and the ikm cannot be swapped.
    #[test]
    fn the_channel_keys_are_hkdf_sha512_salted_with_the_session_id() {
        let isk = [0x11u8; ISK_LEN];
        let sid = session_id(&[7u8; 32], &[9u8; 32]);
        let hmac = |key: &[u8], parts: &[&[u8]]| {
            let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(key).unwrap();
            for part in parts {
                mac.update(part);
            }
            mac.finalize().into_bytes()
        };
        let prk = hmac(&sid, &[&isk]);
        let p2c = hmac(&prk, &[CHANNEL_POND_TO_PHONE_INFO.as_bytes(), &[1]]);
        let c2p = hmac(&prk, &[CHANNEL_PHONE_TO_POND_INFO.as_bytes(), &[1]]);

        let schedule = channel_key_schedule_for(&isk, &sid);
        assert_eq!(schedule.prk.as_slice(), prk.as_slice());
        assert_eq!(
            schedule.keys.pond_to_phone().as_slice(),
            &p2c[..CHANNEL_KEY_LEN]
        );
        assert_eq!(
            schedule.keys.phone_to_pond().as_slice(),
            &c2p[..CHANNEL_KEY_LEN]
        );
    }

    #[test]
    fn channel_identifier_is_length_value_encoded() {
        assert_eq!(
            channel_identifier("pin", "id"),
            [
                &[19u8][..],
                b"goose-pair-cpace-v1",
                &[3],
                b"pin",
                &[2],
                b"id"
            ]
            .concat()
        );
    }

    #[test]
    fn the_setup_channel_identifier_has_its_own_label() {
        assert_eq!(
            setup_channel_identifier("pin", "id"),
            [
                &[20u8][..],
                b"goose-setup-cpace-v1",
                &[3],
                b"pin",
                &[2],
                b"id"
            ]
            .concat()
        );
        assert_ne!(
            setup_channel_identifier(PIN, "phone-1"),
            channel_identifier(PIN, "phone-1")
        );
    }

    #[test]
    fn a_setup_run_and_a_pairing_run_with_the_same_digits_do_not_agree() {
        let code = b"79927398713";
        let pond = Initiator::start(code, &setup_channel_identifier(PIN, "phone-1"), sid());
        let phone = Responder::respond(
            code,
            &channel_identifier(PIN, "phone-1"),
            &sid(),
            &pond.share(),
        )
        .unwrap();
        assert_eq!(
            finish_tag(pond, &phone.share(), &phone.confirmation()),
            Err(CpaceError::InvalidConfirmation)
        );
    }

    // ---- Cross-language exchange vectors --------------------------------------------------

    /// Every value of one fixed run, as `tests/fixtures/cpace-vectors.json` records it.
    fn exchange_run() -> Value {
        let code = "482915";
        let pin = PIN;
        let client_id = "install-abcdef";
        let challenge: Vec<u8> = (0u8..32).collect();
        let client_nonce: Vec<u8> = (32u8..64).collect();
        // The draft's B.3 scalars, so a second implementation can reuse them.
        let ya_bytes = unhex("da3d23700a9e5699258aef94dc060dfda5ebb61f02a5ea77fad53f4ff0976d08");
        let yb_bytes = unhex("d2316b454718c35362d83d69df6320f38578ed5984651435e2949762d900b80d");

        let ci = channel_identifier(pin, client_id);
        let sid = session_id(&challenge, &client_nonce);
        let gen_str = generator_string(code.as_bytes(), &ci, &sid);
        let g = generator(code.as_bytes(), &ci, &sid);
        let pond = Initiator::with_scalar(scalar(&ya_bytes), code.as_bytes(), &ci, sid.clone());
        let ya = pond.share();
        let phone =
            Responder::with_scalar(scalar(&yb_bytes), code.as_bytes(), &ci, &sid, &ya).unwrap();
        let yb = phone.share();
        let k = scalar_mult_vfy(&scalar(&yb_bytes), &ya).unwrap();
        let isk = intermediate_session_key(&sid, k.as_slice(), &ya, &yb);
        let t_client = phone.confirmation();
        let t_server = finish_tag(pond, &yb, &t_client).unwrap();
        assert!(phone.verify_server(&t_server));

        let b64 = |b: &[u8]| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(b)
        };

        // A phone that typed the wrong code, with the same scalar.
        let wrong_code =
            Responder::with_scalar(scalar(&yb_bytes), b"000000", &ci, &sid, &ya).unwrap();
        // A phone that pinned an interceptor's key, with the same scalar.
        let wrong_pin_ci = channel_identifier(OTHER_PIN, client_id);
        let wrong_pin =
            Responder::with_scalar(scalar(&yb_bytes), code.as_bytes(), &wrong_pin_ci, &sid, &ya)
                .unwrap();
        let rejected = |share: &[u8], tag: &[u8]| {
            let pond = Initiator::with_scalar(scalar(&ya_bytes), code.as_bytes(), &ci, sid.clone());
            match finish_tag(pond, share, tag) {
                Ok(_) => "accepted",
                Err(CpaceError::InvalidShare) => "invalid_share",
                Err(CpaceError::InvalidConfirmation) => "invalid_confirmation",
            }
        };
        let non_canonical =
            unhex("2b3c6b8c4f3800e7aef6864025b4ed79bd599117e427c41bd47d93d654b4a51c");

        json!({
            "description": "One fixed CPace pairing run between a Pond (initiator A) and a phone \
                (responder B). Byte strings are lowercase hex; `wire` shows them as the routes \
                carry them. K_mac is the draft's mac_key (section 10.4). Regenerate with CPACE_VECTORS_WRITE=1 cargo test -p pond-infra \
                exchange_vectors.",
            "setup": setup_run(),
            "device": device_run(),
            "spec": "draft-irtf-cfrg-cpace-21",
            "suite": SUITE,
            "labels": {
                "DSI": hex(DSI),
                "DSI_ISK": hex(DSI_ISK),
                "channel": std::str::from_utf8(CHANNEL_LABEL).unwrap(),
                "client_tag": std::str::from_utf8(CLIENT_TAG_LABEL).unwrap(),
                "server_tag": std::str::from_utf8(SERVER_TAG_LABEL).unwrap(),
                "mac_key_prefix": std::str::from_utf8(MAC_KEY_PREFIX).unwrap(),
            },
            "inputs": {
                "code": code,
                "PRS": hex(code.as_bytes()),
                "spki_pin": pin,
                "client_id": client_id,
                "challenge": hex(&challenge),
                "client_nonce": hex(&client_nonce),
                "ya": hex(&ya_bytes),
                "yb": hex(&yb_bytes),
                "ADa": "",
                "ADb": "",
            },
            "outputs": {
                "CI": hex(&ci),
                "sid": hex(&sid),
                "generator_string": hex(&gen_str),
                "g": hex(&g.compress().to_bytes()),
                "Ya": hex(&ya),
                "Yb": hex(&yb),
                "K": hex(k.as_slice()),
                "ISK": hex(isk.as_slice()),
                // The draft's `mac_key`, under a name the history secret scanner leaves alone.
                "K_mac": hex(Confirmation::derive(&sid, k.as_slice(), ya, yb).mac_key.as_slice()),
                "T_client": hex(&t_client),
                "T_server": hex(&t_server),
            },
            "wire": {
                "init_request": {
                    "client_id": client_id,
                    "client_type": "gotg",
                    "client_version": "1.0.0",
                    "channel_binding": pin,
                    "client_nonce": b64(&client_nonce),
                },
                "init_response": {
                    "challenge": b64(&challenge),
                    "suite": SUITE,
                    "share": b64(&ya),
                },
                "verify_request": {
                    "share": b64(&yb),
                    "confirm": hex(&t_client),
                },
                "verify_response": {
                    "accepted": true,
                    "server_proof": hex(&t_server),
                },
            },
            "negative": [
                {
                    "name": "wrong_code",
                    "phone_code": "000000",
                    "Yb": hex(&wrong_code.share()),
                    "T_client": hex(&wrong_code.confirmation()),
                    "pond_result": rejected(&wrong_code.share(), &wrong_code.confirmation()),
                },
                {
                    "name": "wrong_pin",
                    "phone_spki_pin": OTHER_PIN,
                    "CI": hex(&wrong_pin_ci),
                    "Yb": hex(&wrong_pin.share()),
                    "T_client": hex(&wrong_pin.confirmation()),
                    "pond_result": rejected(&wrong_pin.share(), &wrong_pin.confirmation()),
                },
                {
                    "name": "identity_share",
                    "Yb": hex(&[0u8; 32]),
                    "pond_result": rejected(&[0u8; 32], &t_client),
                },
                {
                    "name": "non_canonical_share",
                    "Yb": hex(&non_canonical),
                    "pond_result": rejected(&non_canonical, &t_client),
                },
                {
                    "name": "tampered_server_tag",
                    "T_server": hex(&{ let mut t = t_server; t[0] ^= 1; t }),
                    "phone_result": if phone.verify_server(&{ let mut t = t_server; t[0] ^= 1; t }) { "accepted" } else { "invalid_server_proof" },
                },
            ],
        })
    }

    /// The same exchange with the setup credential: the card's eleven digits as the password and
    /// the setup channel label, under the pairing run's pin, ids and scalars.
    fn setup_run() -> Value {
        let code = "34970112339";
        let pin = PIN;
        let client_id = "install-abcdef";
        let challenge: Vec<u8> = (0u8..32).collect();
        let client_nonce: Vec<u8> = (32u8..64).collect();
        let ya_bytes = unhex("da3d23700a9e5699258aef94dc060dfda5ebb61f02a5ea77fad53f4ff0976d08");
        let yb_bytes = unhex("d2316b454718c35362d83d69df6320f38578ed5984651435e2949762d900b80d");

        let ci = setup_channel_identifier(pin, client_id);
        let sid = session_id(&challenge, &client_nonce);
        let gen_str = generator_string(code.as_bytes(), &ci, &sid);
        let g = generator(code.as_bytes(), &ci, &sid);
        let pond = Initiator::with_scalar(scalar(&ya_bytes), code.as_bytes(), &ci, sid.clone());
        let ya = pond.share();
        let phone =
            Responder::with_scalar(scalar(&yb_bytes), code.as_bytes(), &ci, &sid, &ya).unwrap();
        let yb = phone.share();
        let k = scalar_mult_vfy(&scalar(&yb_bytes), &ya).unwrap();
        let isk = intermediate_session_key(&sid, k.as_slice(), &ya, &yb);
        let t_client = phone.confirmation();
        let t_server = finish_tag(pond, &yb, &t_client).unwrap();
        assert!(phone.verify_server(&t_server));

        let b64 = |b: &[u8]| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(b)
        };
        let rejected = |share: &[u8], tag: &[u8]| {
            let pond = Initiator::with_scalar(scalar(&ya_bytes), code.as_bytes(), &ci, sid.clone());
            match finish_tag(pond, share, tag) {
                Ok(_) => "accepted",
                Err(CpaceError::InvalidShare) => "invalid_share",
                Err(CpaceError::InvalidConfirmation) => "invalid_confirmation",
            }
        };
        // A phone that ran the setup code under the pairing label, with the same scalar.
        let pairing_ci = channel_identifier(pin, client_id);
        let pairing_label =
            Responder::with_scalar(scalar(&yb_bytes), code.as_bytes(), &pairing_ci, &sid, &ya)
                .unwrap();
        // A phone that mistyped one digit of the card, with the same scalar.
        let wrong_code =
            Responder::with_scalar(scalar(&yb_bytes), b"34970112330", &ci, &sid, &ya).unwrap();

        json!({
            "description": "The setup credential: the same exchange as the pairing run above, with \
                the printed card's eleven ASCII digits as PRS and the setup channel label in CI. \
                pake/init carries \"credential\": \"setup\"; everything else on the wire is the \
                pairing exchange unchanged.",
            "credential": "setup",
            "labels": {
                "channel": std::str::from_utf8(SETUP_CHANNEL_LABEL).unwrap(),
            },
            "inputs": {
                "code": code,
                "PRS": hex(code.as_bytes()),
                "spki_pin": pin,
                "client_id": client_id,
                "challenge": hex(&challenge),
                "client_nonce": hex(&client_nonce),
                "ya": hex(&ya_bytes),
                "yb": hex(&yb_bytes),
                "ADa": "",
                "ADb": "",
            },
            "outputs": {
                "CI": hex(&ci),
                "sid": hex(&sid),
                "generator_string": hex(&gen_str),
                "g": hex(&g.compress().to_bytes()),
                "Ya": hex(&ya),
                "Yb": hex(&yb),
                "K": hex(k.as_slice()),
                "ISK": hex(isk.as_slice()),
                "K_mac": hex(Confirmation::derive(&sid, k.as_slice(), ya, yb).mac_key.as_slice()),
                "T_client": hex(&t_client),
                "T_server": hex(&t_server),
            },
            "wire": {
                "init_request": {
                    "client_id": client_id,
                    "client_type": "gotg",
                    "client_version": "1.0.0",
                    "channel_binding": pin,
                    "client_nonce": b64(&client_nonce),
                    "credential": "setup",
                },
                "init_response": {
                    "challenge": b64(&challenge),
                    "suite": SUITE,
                    "share": b64(&ya),
                },
                "verify_request": {
                    "share": b64(&yb),
                    "confirm": hex(&t_client),
                },
                "verify_response": {
                    "accepted": true,
                    "server_proof": hex(&t_server),
                },
            },
            "negative": [
                {
                    "name": "pairing_label",
                    "CI": hex(&pairing_ci),
                    "Yb": hex(&pairing_label.share()),
                    "T_client": hex(&pairing_label.confirmation()),
                    "pond_result": rejected(&pairing_label.share(), &pairing_label.confirmation()),
                },
                {
                    "name": "wrong_code",
                    "phone_code": "34970112330",
                    "Yb": hex(&wrong_code.share()),
                    "T_client": hex(&wrong_code.confirmation()),
                    "pond_result": rejected(&wrong_code.share(), &wrong_code.confirmation()),
                },
            ],
        })
    }

    /// A paired phone resuming over the setup channel, under the pairing run's pin, ids and
    /// scalars. The token is the one the setup channel vectors' sealed answer hands the phone.
    fn device_run() -> Value {
        let refresh_token = "vector-refresh-0001";
        let refresh_hash = device_password(refresh_token);
        let pin = PIN;
        let client_id = "install-abcdef";
        let challenge: Vec<u8> = (0u8..32).collect();
        let client_nonce: Vec<u8> = (32u8..64).collect();
        let ya_bytes = unhex("da3d23700a9e5699258aef94dc060dfda5ebb61f02a5ea77fad53f4ff0976d08");
        let yb_bytes = unhex("d2316b454718c35362d83d69df6320f38578ed5984651435e2949762d900b80d");
        let prs = refresh_hash.as_bytes();

        let ci = device_channel_identifier(pin, client_id);
        let sid = session_id(&challenge, &client_nonce);
        let gen_str = generator_string(prs, &ci, &sid);
        let g = generator(prs, &ci, &sid);
        let pond = Initiator::with_scalar(scalar(&ya_bytes), prs, &ci, sid.clone());
        let ya = pond.share();
        let phone = Responder::with_scalar(scalar(&yb_bytes), prs, &ci, &sid, &ya).unwrap();
        let yb = phone.share();
        let k = scalar_mult_vfy(&scalar(&yb_bytes), &ya).unwrap();
        let isk = intermediate_session_key(&sid, k.as_slice(), &ya, &yb);
        let t_client = phone.confirmation();
        let t_server = finish_tag(pond, &yb, &t_client).unwrap();
        assert!(phone.verify_server(&t_server));

        let b64 = |b: &[u8]| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(b)
        };
        let rejected = |share: &[u8], tag: &[u8]| {
            let pond = Initiator::with_scalar(scalar(&ya_bytes), prs, &ci, sid.clone());
            match finish_tag(pond, share, tag) {
                Ok(_) => "accepted",
                Err(CpaceError::InvalidShare) => "invalid_share",
                Err(CpaceError::InvalidConfirmation) => "invalid_confirmation",
            }
        };
        // The phone's likeliest mistakes, each with the same scalar: the digest's raw bytes
        // instead of its hex, upper-case hex, and the right hash under the setup label.
        let raw_digest = sha2::Sha256::digest(refresh_token.as_bytes()).to_vec();
        let raw = Responder::with_scalar(scalar(&yb_bytes), &raw_digest, &ci, &sid, &ya).unwrap();
        let upper_hash = refresh_hash.to_ascii_uppercase();
        let upper =
            Responder::with_scalar(scalar(&yb_bytes), upper_hash.as_bytes(), &ci, &sid, &ya)
                .unwrap();
        let setup_ci = setup_channel_identifier(pin, client_id);
        let setup_label =
            Responder::with_scalar(scalar(&yb_bytes), prs, &setup_ci, &sid, &ya).unwrap();

        json!({
            "description": "A paired phone resuming over the setup channel: the same exchange as \
                the pairing run above, with the lowercase hex SHA-256 of the phone's current \
                refresh token as the password and the device channel label in CI. PRS is the 64 \
                ASCII characters of refresh_hash, written in hex like every PRS here; the digest's \
                32 raw bytes are not the password (negative raw_digest). pake_init carries \
                \"credential\": \"device\"; everything else on the wire is the pairing exchange \
                unchanged. The setup channel only: HTTPS answers invalid_credential.",
            "credential": "device",
            "labels": {
                "channel": std::str::from_utf8(DEVICE_CHANNEL_LABEL).unwrap(),
            },
            "inputs": {
                "refresh_token": refresh_token,
                "refresh_hash": refresh_hash.as_str(),
                "PRS": hex(prs),
                "spki_pin": pin,
                "client_id": client_id,
                "challenge": hex(&challenge),
                "client_nonce": hex(&client_nonce),
                "ya": hex(&ya_bytes),
                "yb": hex(&yb_bytes),
                "ADa": "",
                "ADb": "",
            },
            "outputs": {
                "CI": hex(&ci),
                "sid": hex(&sid),
                "generator_string": hex(&gen_str),
                "g": hex(&g.compress().to_bytes()),
                "Ya": hex(&ya),
                "Yb": hex(&yb),
                "K": hex(k.as_slice()),
                "ISK": hex(isk.as_slice()),
                "K_mac": hex(Confirmation::derive(&sid, k.as_slice(), ya, yb).mac_key.as_slice()),
                "T_client": hex(&t_client),
                "T_server": hex(&t_server),
            },
            "wire": {
                "init_request": {
                    "client_id": client_id,
                    "client_type": "gotg",
                    "client_version": "1.0.0",
                    "channel_binding": pin,
                    "client_nonce": b64(&client_nonce),
                    "credential": "device",
                },
                "init_response": {
                    "challenge": b64(&challenge),
                    "suite": SUITE,
                    "share": b64(&ya),
                },
                "verify_request": {
                    "share": b64(&yb),
                    "confirm": hex(&t_client),
                },
                "verify_response": {
                    "accepted": true,
                    "server_proof": hex(&t_server),
                },
            },
            "negative": [
                {
                    "name": "raw_digest",
                    "phone_PRS": hex(&raw_digest),
                    "Yb": hex(&raw.share()),
                    "T_client": hex(&raw.confirmation()),
                    "pond_result": rejected(&raw.share(), &raw.confirmation()),
                },
                {
                    "name": "uppercase_hash",
                    "phone_PRS": hex(upper_hash.as_bytes()),
                    "Yb": hex(&upper.share()),
                    "T_client": hex(&upper.confirmation()),
                    "pond_result": rejected(&upper.share(), &upper.confirmation()),
                },
                {
                    "name": "setup_label",
                    "CI": hex(&setup_ci),
                    "Yb": hex(&setup_label.share()),
                    "T_client": hex(&setup_label.confirmation()),
                    "pond_result": rejected(&setup_label.share(), &setup_label.confirmation()),
                },
            ],
        })
    }

    #[test]
    fn the_device_vector_is_the_refresh_hash_under_its_own_label() {
        let run = exchange_run();
        let device = &run["device"];
        assert_eq!(device["credential"], "device");
        assert_eq!(device["labels"]["channel"], "goose-device-cpace-v1");
        assert_eq!(device["wire"]["init_request"]["credential"], "device");
        let token = device["inputs"]["refresh_token"].as_str().unwrap();
        let expected_hash = hex(&sha2::Sha256::digest(token.as_bytes()));
        assert_eq!(device["inputs"]["refresh_hash"], expected_hash.as_str());
        assert_eq!(
            device["inputs"]["PRS"],
            hex(expected_hash.as_bytes()).as_str(),
            "the password is the hash's 64 ASCII characters"
        );
        for other in [&run, &run["setup"]] {
            assert_ne!(device["outputs"]["CI"], other["outputs"]["CI"]);
            assert_ne!(device["outputs"]["g"], other["outputs"]["g"]);
            assert_ne!(device["outputs"]["ISK"], other["outputs"]["ISK"]);
        }
        for negative in device["negative"].as_array().unwrap() {
            assert_eq!(
                negative["pond_result"], "invalid_confirmation",
                "{negative}"
            );
        }
    }

    #[test]
    fn the_device_channel_identifier_has_its_own_label() {
        assert_eq!(
            device_channel_identifier("pin", "id"),
            [
                &[21u8][..],
                b"goose-device-cpace-v1",
                &[3],
                b"pin",
                &[2],
                b"id"
            ]
            .concat()
        );
        assert_ne!(
            device_channel_identifier(PIN, "phone-1"),
            setup_channel_identifier(PIN, "phone-1")
        );
    }

    #[test]
    fn the_setup_vector_is_marked_and_differs_from_the_pairing_run() {
        let run = exchange_run();
        let setup = &run["setup"];
        assert_eq!(setup["credential"], "setup");
        assert_eq!(setup["labels"]["channel"], "goose-setup-cpace-v1");
        assert_eq!(setup["wire"]["init_request"]["credential"], "setup");
        assert_eq!(setup["inputs"]["code"], "34970112339");
        assert!(
            pond_core::network::domain::setup_code::SetupCode::parse("34970112339").is_ok(),
            "the vector's code passes the card's own check digit"
        );
        assert_ne!(setup["outputs"]["CI"], run["outputs"]["CI"]);
        assert_ne!(setup["outputs"]["g"], run["outputs"]["g"]);
        for negative in setup["negative"].as_array().unwrap() {
            assert_eq!(
                negative["pond_result"], "invalid_confirmation",
                "{negative}"
            );
        }
        assert!(
            run["wire"]["init_request"].get("credential").is_none(),
            "an ordinary pairing omits the credential"
        );
    }

    #[test]
    fn exchange_vectors_match_the_fixture() {
        let path = fixture("cpace-vectors.json");
        let expected = exchange_run();
        if std::env::var_os("CPACE_VECTORS_WRITE").is_some() {
            let mut text = serde_json::to_string_pretty(&expected).unwrap();
            text.push('\n');
            std::fs::write(&path, text).unwrap();
        }
        let recorded: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .expect("the vector file is JSON");
        assert_eq!(
            recorded,
            expected,
            "{} no longer matches this implementation; the app checks the same file, so a \
             change here is a protocol change",
            path.display()
        );
    }
}
