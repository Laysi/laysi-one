//! Signed release manifests: which images make up a release.
//!
//! A release is three images on ghcr.io: api, web and backup. The `mcp`
//! service runs the api image. A tag on the registry can be moved, and a
//! version string cannot say which three images belong together, so the
//! release workflow signs one small document per `v*` tag that names the
//! index digest of each image. An installation runs only images whose digest
//! a manifest it verified names.
//!
//! The manifest key is not the licence key. The licence server holds the
//! licence private key; if that key also chose what every installation runs,
//! breaking into the licence server would be code execution on every machine
//! sold. The manifest private key is a CI secret, and nothing in this crate
//! can sign: the licence server links this crate to check and carry
//! manifests, and producing one needs a key it never has.
//!
//! The format is text, and the signature covers exact bytes:
//!
//! ```text
//! laysi-release-manifest 1
//! version v0.1.13
//! image api sha256:<64 lowercase hex>
//! image web sha256:<64 lowercase hex>
//! image backup sha256:<64 lowercase hex>
//! signature <key id, 16 lowercase hex> <Ed25519 signature, 128 lowercase hex>
//! ```
//!
//! The body is the first five lines, each with its newline. Every signature
//! line is over exactly those bytes, so a signer in any language agrees with
//! this verifier without a canonical form. A licence token's signed half is
//! base64url text, and every manifest body starts with a header containing a
//! space, so neither kind of signed message can stand in for the other even
//! under one key.
//!
//! One to many signature lines may follow. Lines from keys this build does not
//! trust are skipped; every line from a trusted key must verify; at least one
//! must come from a trusted key. Two keys signing together is how the key
//! gets rotated without stranding installations that skipped a release.

use std::cmp::Ordering;
use std::fmt;

use ed25519_dalek::Signature;
pub use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};

/// Public keys a release manifest may be signed by, 64 lowercase hex each.
///
/// Compiled in, and kept apart from the licence key in the product's
/// `ops/license.rs`. release.yml derives the public key from the signing
/// secret and fails the release before any image is built when it is not in
/// this list. It reads every quoted 64-hex string from the line that declares
/// this constant to the `];` that closes it, so a rename fails the release
/// rather than skipping the check.
pub const TRUSTED_KEYS: &[&str] =
    &["000aef2b6ed55b8cfb5481af7bce7b0e65584aaec863073348c68e132d1021a5"];

/// The largest manifest anything should read. A manifest with eight
/// signatures is under 1.6 KB; enforce this before reading a file or a
/// response body into memory, and [`verify`] refuses anything larger.
pub const MAX_MANIFEST_BYTES: usize = 4096;

const FORMAT: &str = "laysi-release-manifest";
const HEADER: &str = "laysi-release-manifest 1";

/// One of the images a release is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Image {
    /// `ghcr.io/laysi/laysi-one-api`, run by the `api` and `mcp` services.
    Api,
    /// `ghcr.io/laysi/laysi-one-web`.
    Web,
    /// `ghcr.io/laysi/laysi-one-backup`.
    Backup,
}

impl Image {
    /// Every image, in the order the manifest lists them.
    pub const ALL: [Image; 3] = [Image::Api, Image::Web, Image::Backup];

    /// The name in the manifest, which is also the repository suffix after
    /// `laysi-one-`.
    pub fn name(self) -> &'static str {
        match self {
            Image::Api => "api",
            Image::Web => "web",
            Image::Backup => "backup",
        }
    }

    pub fn from_name(name: &str) -> Option<Image> {
        Image::ALL.into_iter().find(|image| image.name() == name)
    }
}

impl fmt::Display for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// An OCI content digest, `sha256:` and 64 lowercase hex characters.
///
/// The manifest pins the index digest of each multi-platform image: what a
/// pull by tag resolves to, and what to pull by instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageDigest([u8; 32]);

impl ImageDigest {
    /// Read a digest in the one form the OCI image spec allows for sha256.
    /// Uppercase hex, another algorithm or a bare hash is `None`.
    pub fn parse(text: &str) -> Option<ImageDigest> {
        decode_hex(text.strip_prefix("sha256:")?).map(ImageDigest)
    }

    /// Whether a digest reported for an image on the host is this one.
    /// Anything that does not parse as a digest does not match.
    pub fn matches(&self, reported: &str) -> bool {
        ImageDigest::parse(reported.trim()) == Some(*self)
    }
}

impl fmt::Display for ImageDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        write_hex(f, &self.0)
    }
}

/// The first 8 bytes of the SHA-256 of a raw 32-byte public key.
///
/// Derived rather than named, so two keys cannot be given one id. It only
/// selects which trusted key a signature line is checked against; the
/// signature is always verified with the full key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyId([u8; 8]);

impl KeyId {
    pub fn of(key: &VerifyingKey) -> KeyId {
        let digest = Sha256::digest(key.as_bytes());
        let mut id = [0u8; 8];
        id.copy_from_slice(&digest[..8]);
        KeyId(id)
    }

    fn parse(text: &str) -> Option<KeyId> {
        decode_hex(text).map(KeyId)
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

/// Read a public key written as 64 lowercase hex characters.
pub fn parse_public_key(text: &str) -> Option<VerifyingKey> {
    VerifyingKey::from_bytes(&decode_hex(text)?).ok()
}

/// The keys in [`TRUSTED_KEYS`]. One that does not parse is left out, which
/// can only make verification stricter.
pub fn trusted_keys() -> Vec<VerifyingKey> {
    TRUSTED_KEYS
        .iter()
        .filter_map(|hex| parse_public_key(hex))
        .collect()
}

/// A release version: the tag release.yml was run for, which is also what
/// `/v1/meta` reports and what the licence server records.
///
/// `v`, then `MAJOR.MINOR.PATCH` with an optional pre-release, ordered the way
/// semantic versioning orders them (`v1.0.0-rc.2` < `v1.0.0-rc.10` <
/// `v1.0.0`). Leading zeros and build metadata are refused: either would let
/// two different strings name the same release.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReleaseVersion {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<PreRelease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PreRelease {
    Numeric(u64),
    Alphanumeric(String),
}

impl ReleaseVersion {
    pub fn parse(text: &str) -> Result<ReleaseVersion, VersionError> {
        let rest = text.strip_prefix('v').ok_or(VersionError)?;
        let (core, pre) = match rest.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (rest, None),
        };
        let mut parts = core.split('.');
        let mut number = || parts.next().and_then(numeric).ok_or(VersionError);
        let (major, minor, patch) = (number()?, number()?, number()?);
        if parts.next().is_some() {
            return Err(VersionError);
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|id| {
                    if let Some(n) = numeric(id) {
                        Ok(PreRelease::Numeric(n))
                    } else if !id.is_empty()
                        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        && !id.bytes().all(|b| b.is_ascii_digit())
                    {
                        Ok(PreRelease::Alphanumeric(id.to_string()))
                    } else {
                        Err(VersionError)
                    }
                })
                .collect::<Result<_, _>>()?,
        };
        Ok(ReleaseVersion {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

/// Digits without a leading zero, as semantic versioning requires.
fn numeric(text: &str) -> Option<u64> {
    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    text.parse().ok()
}

impl Ord for ReleaseVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for ReleaseVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PreRelease {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (PreRelease::Numeric(a), PreRelease::Numeric(b)) => a.cmp(b),
            (PreRelease::Numeric(_), PreRelease::Alphanumeric(_)) => Ordering::Less,
            (PreRelease::Alphanumeric(_), PreRelease::Numeric(_)) => Ordering::Greater,
            (PreRelease::Alphanumeric(a), PreRelease::Alphanumeric(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for PreRelease {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, id) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match id {
                PreRelease::Numeric(n) => write!(f, "{n}")?,
                PreRelease::Alphanumeric(s) => f.write_str(s)?,
            }
        }
        Ok(())
    }
}

/// A string that is not a release version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionError;

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("版本號格式應為 v1.2.3")
    }
}

impl std::error::Error for VersionError {}

/// A manifest whose signature checked out against the keys it was verified
/// with. [`verify`] and [`verify_with`] are the only way to get one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManifest {
    version: ReleaseVersion,
    digests: [ImageDigest; 3],
    signed_by: Vec<KeyId>,
}

impl ReleaseManifest {
    pub fn version(&self) -> &ReleaseVersion {
        &self.version
    }

    pub fn digest(&self, image: Image) -> ImageDigest {
        self.digests[image as usize]
    }

    /// Every image with its digest, in manifest order.
    pub fn digests(&self) -> impl Iterator<Item = (Image, ImageDigest)> + '_ {
        Image::ALL
            .into_iter()
            .map(|image| (image, self.digest(image)))
    }

    /// The trusted keys whose signatures verified, in the order they appear.
    pub fn signed_by(&self) -> &[KeyId] {
        &self.signed_by
    }

    /// Whether to install this release over the one named `installed`.
    ///
    /// Only a later release is installed. An earlier one is ignored with the
    /// reason to record, so replaying an old manifest does nothing; an
    /// installed version that does not parse is a refusal, never a reason to
    /// treat the manifest as newer.
    pub fn decide(&self, installed: &str) -> Decision {
        let Ok(current) = ReleaseVersion::parse(installed.trim()) else {
            return Decision::Ignore(Refusal::UnreadableInstalled {
                installed: installed.to_string(),
            });
        };
        match self.version.cmp(&current) {
            Ordering::Greater => Decision::Install,
            Ordering::Equal => Decision::UpToDate,
            Ordering::Less => Decision::Ignore(Refusal::Downgrade {
                installed: current,
                offered: self.version.clone(),
            }),
        }
    }

    /// Check the digest the host reports for an image before it is started.
    pub fn check_image(&self, image: Image, reported: &str) -> Result<(), DigestMismatch> {
        let expected = self.digest(image);
        if expected.matches(reported) {
            Ok(())
        } else {
            Err(DigestMismatch {
                image,
                expected,
                reported: reported.to_string(),
            })
        }
    }
}

/// What to do with a verified manifest, given the installed release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The manifest names a later release.
    Install,
    /// The manifest names the release already installed.
    UpToDate,
    /// Not installed; the reason is what gets recorded.
    Ignore(Refusal),
}

/// Why a verified manifest is not installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Downgrade {
        installed: ReleaseVersion,
        offered: ReleaseVersion,
    },
    UnreadableInstalled {
        installed: String,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Downgrade { installed, offered } => {
                write!(f, "版本清單的 {offered} 低於已安裝的 {installed}")
            }
            Refusal::UnreadableInstalled { installed } => {
                write!(f, "無法辨識已安裝的版本「{installed}」")
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// An image on the host whose digest is not the one the manifest names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestMismatch {
    pub image: Image,
    pub expected: ImageDigest,
    pub reported: String,
}

impl fmt::Display for DigestMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} 映像的 digest 與版本清單不符：清單為 {}，主機上為「{}」",
            self.image, self.expected, self.reported
        )
    }
}

impl std::error::Error for DigestMismatch {}

/// Why a manifest did not verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// Longer than [`MAX_MANIFEST_BYTES`].
    TooLarge { len: usize },
    /// A line, counted from 1, is not what the format puts there. Also a
    /// missing final newline, a carriage return or an empty line.
    Malformed { line: usize },
    /// A manifest format this build does not read.
    UnsupportedFormat,
    /// The version line does not hold a release version.
    Version,
    /// The digest for an image is not `sha256:` and 64 lowercase hex.
    Digest { image: Image },
    /// No signature lines at all.
    Unsigned,
    /// Two signature lines name the same key.
    DuplicateKeyId(KeyId),
    /// None of the signatures is from a trusted key.
    NoTrustedSignature,
    /// A trusted key's signature does not match the body.
    BadSignature(KeyId),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::TooLarge { len } => {
                write!(
                    f,
                    "版本清單有 {len} 位元組，上限為 {MAX_MANIFEST_BYTES} 位元組"
                )
            }
            ManifestError::Malformed { line } => write!(f, "版本清單第 {line} 行格式不符"),
            ManifestError::UnsupportedFormat => {
                f.write_str("版本清單的格式版本較新，請先手動更新本系統")
            }
            ManifestError::Version => f.write_str("版本清單的版本號格式應為 v1.2.3"),
            ManifestError::Digest { image } => {
                write!(f, "版本清單中 {image} 映像的 digest 格式不符")
            }
            ManifestError::Unsigned => f.write_str("版本清單沒有簽章"),
            ManifestError::DuplicateKeyId(id) => {
                write!(f, "版本清單有兩個金鑰 {id} 的簽章")
            }
            ManifestError::NoTrustedSignature => f.write_str("版本清單沒有本系統信任的金鑰簽章"),
            ManifestError::BadSignature(id) => {
                write!(f, "金鑰 {id} 的簽章與版本清單內容不符")
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Verify a manifest against the keys compiled into this build.
pub fn verify(text: &str) -> Result<ReleaseManifest, ManifestError> {
    verify_with(text, &trusted_keys())
}

/// Verify a manifest against the given keys. For tests, which sign with keys
/// of their own; everything that decides what runs calls [`verify`].
pub fn verify_with(text: &str, trusted: &[VerifyingKey]) -> Result<ReleaseManifest, ManifestError> {
    if text.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge { len: text.len() });
    }

    // Every line ends in a newline, including the last, and none is empty or
    // carries a carriage return. The bytes are what was signed, so anything
    // that rewrote them is refused rather than repaired.
    let mut lines = Vec::new();
    let mut offset = 0;
    for (i, piece) in text.split_inclusive('\n').enumerate() {
        let line = piece
            .strip_suffix('\n')
            .filter(|line| !line.is_empty() && !line.contains('\r'))
            .ok_or(ManifestError::Malformed { line: i + 1 })?;
        offset += piece.len();
        lines.push((line, offset));
    }

    let header = lines.first().ok_or(ManifestError::Malformed { line: 1 })?.0;
    if header != HEADER {
        return Err(match header.split_once(' ') {
            Some((FORMAT, _)) => ManifestError::UnsupportedFormat,
            _ => ManifestError::Malformed { line: 1 },
        });
    }
    if lines.len() < 5 {
        return Err(ManifestError::Malformed {
            line: lines.len() + 1,
        });
    }

    let version = lines[1]
        .0
        .strip_prefix("version ")
        .ok_or(ManifestError::Malformed { line: 2 })?;
    let version = ReleaseVersion::parse(version).map_err(|_| ManifestError::Version)?;

    let mut digests = [ImageDigest([0; 32]); 3];
    for (i, image) in Image::ALL.into_iter().enumerate() {
        let line = i + 3;
        let mut fields = lines[line - 1].0.split(' ');
        let (Some("image"), Some(name), Some(digest), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(ManifestError::Malformed { line });
        };
        if name != image.name() {
            return Err(ManifestError::Malformed { line });
        }
        digests[i] = ImageDigest::parse(digest).ok_or(ManifestError::Digest { image })?;
    }

    let body = &text[..lines[4].1];
    let mut seen = Vec::new();
    let mut signed_by = Vec::new();
    for (i, (line, _)) in lines.iter().enumerate().skip(5) {
        let malformed = ManifestError::Malformed { line: i + 1 };
        let mut fields = line.split(' ');
        let (Some("signature"), Some(id), Some(sig), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(malformed);
        };
        let id = KeyId::parse(id).ok_or(malformed.clone())?;
        let sig = Signature::from_bytes(&decode_hex::<64>(sig).ok_or(malformed)?);
        if seen.contains(&id) {
            return Err(ManifestError::DuplicateKeyId(id));
        }
        seen.push(id);
        if let Some(key) = trusted.iter().find(|key| KeyId::of(key) == id) {
            key.verify_strict(body.as_bytes(), &sig)
                .map_err(|_| ManifestError::BadSignature(id))?;
            signed_by.push(id);
        }
    }

    if seen.is_empty() {
        return Err(ManifestError::Unsigned);
    }
    if signed_by.is_empty() {
        return Err(ManifestError::NoTrustedSignature);
    }
    Ok(ReleaseManifest {
        version,
        digests,
        signed_by,
    })
}

/// Lowercase hex only: one spelling per value, so the bytes that were signed
/// have one textual form.
fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (byte, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        *byte = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(out)
}

fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    bytes.iter().try_for_each(|b| write!(f, "{b:02x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const API: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const WEB: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const BACKUP: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn body(version: &str) -> String {
        format!("{HEADER}\nversion {version}\nimage api {API}\nimage web {WEB}\nimage backup {BACKUP}\n")
    }

    fn signature(key: &SigningKey, body: &str) -> String {
        format!(
            "signature {} {}\n",
            KeyId::of(&key.verifying_key()),
            hex(&key.sign(body.as_bytes()).to_bytes())
        )
    }

    fn signed(body: &str, keys: &[&SigningKey]) -> String {
        let mut out = body.to_string();
        for key in keys {
            out.push_str(&signature(key, body));
        }
        out
    }

    fn trusting(keys: &[&SigningKey]) -> Vec<VerifyingKey> {
        keys.iter().map(|k| k.verifying_key()).collect()
    }

    #[test]
    fn a_manifest_signed_by_a_trusted_key_verifies() {
        let k = key(7);
        let text = signed(&body("v0.1.13"), &[&k]);
        let m = verify_with(&text, &trusting(&[&k])).expect("應驗得過");

        assert_eq!(m.version().to_string(), "v0.1.13");
        assert_eq!(m.digest(Image::Api).to_string(), API);
        assert_eq!(m.digest(Image::Web).to_string(), WEB);
        assert_eq!(m.digest(Image::Backup).to_string(), BACKUP);
        let listed: Vec<_> = m
            .digests()
            .map(|(i, d)| (i.name(), d.to_string()))
            .collect();
        assert_eq!(
            listed,
            [
                ("api", API.to_string()),
                ("web", WEB.to_string()),
                ("backup", BACKUP.to_string())
            ]
        );
        assert_eq!(m.signed_by(), &[KeyId::of(&k.verifying_key())]);
    }

    #[test]
    fn a_manifest_signed_with_the_wrong_key_is_rejected() {
        let trusted = key(7);
        let other = key(8);

        // Signed by a key nobody trusts.
        let text = signed(&body("v0.1.13"), &[&other]);
        assert_eq!(
            verify_with(&text, &trusting(&[&trusted])),
            Err(ManifestError::NoTrustedSignature)
        );

        // Signed by another key but claiming the trusted key's id.
        let forged = format!(
            "{}signature {} {}\n",
            body("v0.1.13"),
            KeyId::of(&trusted.verifying_key()),
            hex(&other.sign(body("v0.1.13").as_bytes()).to_bytes())
        );
        assert_eq!(
            verify_with(&forged, &trusting(&[&trusted])),
            Err(ManifestError::BadSignature(KeyId::of(
                &trusted.verifying_key()
            )))
        );
    }

    #[test]
    fn a_trusted_and_an_unknown_signature_together_verify() {
        let trusted = key(7);
        let unknown = key(8);
        for keys in [[&trusted, &unknown], [&unknown, &trusted]] {
            let text = signed(&body("v0.1.13"), &keys);
            let m = verify_with(&text, &trusting(&[&trusted])).expect("認不得的簽章應略過");
            assert_eq!(m.signed_by(), &[KeyId::of(&trusted.verifying_key())]);
        }

        // Both trusted: both are checked and both are reported.
        let text = signed(&body("v0.1.13"), &[&trusted, &unknown]);
        let m = verify_with(&text, &trusting(&[&trusted, &unknown])).unwrap();
        assert_eq!(m.signed_by().len(), 2);
    }

    #[test]
    fn only_unknown_signatures_are_rejected() {
        let text = signed(&body("v0.1.13"), &[&key(8), &key(9)]);
        assert_eq!(
            verify_with(&text, &trusting(&[&key(7)])),
            Err(ManifestError::NoTrustedSignature)
        );
    }

    #[test]
    fn an_unsigned_manifest_is_rejected() {
        assert_eq!(
            verify_with(&body("v0.1.13"), &trusting(&[&key(7)])),
            Err(ManifestError::Unsigned)
        );
    }

    #[test]
    fn nothing_verifies_against_no_keys() {
        let text = signed(&body("v0.1.13"), &[&key(7)]);
        assert_eq!(
            verify_with(&text, &[]),
            Err(ManifestError::NoTrustedSignature)
        );
    }

    #[test]
    fn an_edited_body_is_rejected() {
        let k = key(7);
        let text = signed(&body("v0.1.13"), &[&k]);
        let id = KeyId::of(&k.verifying_key());

        // Pointing an image at different content.
        let other_api = API.replace('1', "4");
        let edited = text.replace(API, &other_api);
        assert_eq!(
            verify_with(&edited, &trusting(&[&k])),
            Err(ManifestError::BadSignature(id))
        );

        // Relabelling the release.
        let edited = text.replace("v0.1.13", "v0.1.14");
        assert_eq!(
            verify_with(&edited, &trusting(&[&k])),
            Err(ManifestError::BadSignature(id))
        );
    }

    #[test]
    fn a_trusted_signature_that_fails_rejects_the_manifest_even_beside_a_good_one() {
        let (a, b) = (key(7), key(8));
        let good = signed(&body("v0.1.13"), &[&a]);
        let stale = signature(&b, &body("v0.1.12"));
        let text = format!("{good}{stale}");
        assert_eq!(
            verify_with(&text, &trusting(&[&a, &b])),
            Err(ManifestError::BadSignature(KeyId::of(&b.verifying_key())))
        );
    }

    #[test]
    fn malformed_signature_lines_are_rejected() {
        let k = key(7);
        let b = body("v0.1.13");
        let good = signed(&b, &[&k]);
        let line = signature(&k, &b);
        let (_, rest) = line.trim_end().split_once(' ').unwrap();
        let (id, sig) = rest.split_once(' ').unwrap();
        let trusted = trusting(&[&k]);
        let malformed = Err(ManifestError::Malformed { line: 6 });

        let cases = [
            format!("{b}signature {} {sig}\n", &id[..15]),
            format!("{b}signature {}0 {sig}\n", id),
            format!("{b}signature {} {sig}\n", id.to_uppercase()),
            format!("{b}signature {id} {}\n", &sig[..127]),
            format!("{b}signature {id} {}\n", sig.to_uppercase()),
            format!("{b}signature {id} {sig} extra\n"),
            format!("{b}signature  {id} {sig}\n"),
            format!("{b}sig {id} {sig}\n"),
            format!("{b}signature {id} {sig}\r\n"),
            format!("{b}\n"),
        ];
        for case in &cases {
            assert_eq!(verify_with(case, &trusted), malformed, "{case:?}");
        }

        // No final newline: the bytes were not forwarded as they were signed.
        assert_eq!(
            verify_with(good.trim_end(), &trusted),
            Err(ManifestError::Malformed { line: 6 })
        );

        // The same key twice.
        let twice = format!("{good}{line}");
        assert_eq!(
            verify_with(&twice, &trusted),
            Err(ManifestError::DuplicateKeyId(KeyId::of(&k.verifying_key())))
        );
    }

    #[test]
    fn malformed_digests_are_rejected() {
        let k = key(7);
        let trusted = trusting(&[&k]);
        let bad = [
            API.replace("sha256:", "sha512:"),
            API.replace("sha256:", ""),
            API.replace("sha256:", "SHA256:"),
            API.replace('1', "A"),
            API.replace('1', "g"),
            API[..API.len() - 1].to_string(),
            format!("{API}1"),
        ];
        for digest in &bad {
            let b = body("v0.1.13").replace(API, digest);
            assert_eq!(
                verify_with(&signed(&b, &[&k]), &trusted),
                Err(ManifestError::Digest { image: Image::Api }),
                "{digest}"
            );
            assert!(ImageDigest::parse(digest).is_none(), "{digest}");
        }
    }

    #[test]
    fn the_body_has_one_shape() {
        let k = key(7);
        let trusted = trusting(&[&k]);
        let check = |b: String| verify_with(&signed(&b, &[&k]), &trusted);
        let b = body("v0.1.13");

        // A newer format is named as such, so the reason recorded says to
        // update by hand rather than that the manifest is broken.
        assert_eq!(
            check(b.replace(HEADER, "laysi-release-manifest 2")),
            Err(ManifestError::UnsupportedFormat)
        );
        assert_eq!(
            check(b.replace(HEADER, "laysi-manifest 1")),
            Err(ManifestError::Malformed { line: 1 })
        );
        assert_eq!(
            check(b.replace("version ", "release ")),
            Err(ManifestError::Malformed { line: 2 })
        );
        assert_eq!(
            check(b.replace("v0.1.13", "0.1.13")),
            Err(ManifestError::Version)
        );

        // Images in another order, missing, renamed or one too many.
        let swapped = b
            .replace("image api", "image tmp")
            .replace("image web", "image api")
            .replace("image tmp", "image web");
        assert_eq!(check(swapped), Err(ManifestError::Malformed { line: 3 }));
        let no_backup = b.replace(&format!("image backup {BACKUP}\n"), "");
        assert_eq!(
            check(no_backup.clone()),
            Err(ManifestError::Malformed { line: 5 })
        );
        assert_eq!(
            check(b.replace("image web", "image mcp")),
            Err(ManifestError::Malformed { line: 4 })
        );
        let extra = format!("{b}image worker {API}\n");
        assert_eq!(check(extra), Err(ManifestError::Malformed { line: 6 }));
        assert_eq!(
            check(b.replace("image api ", "image  api ")),
            Err(ManifestError::Malformed { line: 3 })
        );

        // Too short to have a body at all.
        assert_eq!(
            verify_with(&format!("{HEADER}\n"), &trusted),
            Err(ManifestError::Malformed { line: 2 })
        );
        assert_eq!(
            verify_with("", &trusted),
            Err(ManifestError::Malformed { line: 1 })
        );
    }

    #[test]
    fn oversize_input_is_rejected_before_anything_else() {
        let k = key(7);
        let mut text = signed(&body("v0.1.13"), &[&k]);
        let mut seed = 10;
        while text.len() <= MAX_MANIFEST_BYTES {
            text.push_str(&signature(&key(seed), &body("v0.1.13")));
            seed += 1;
        }
        assert_eq!(
            verify_with(&text, &trusting(&[&k])),
            Err(ManifestError::TooLarge { len: text.len() })
        );

        // Just under the limit is read normally.
        let mut fits = signed(&body("v0.1.13"), &[&k]);
        let mut seed = 10;
        loop {
            let next = signature(&key(seed), &body("v0.1.13"));
            if fits.len() + next.len() > MAX_MANIFEST_BYTES {
                break;
            }
            fits.push_str(&next);
            seed += 1;
        }
        assert!(verify_with(&fits, &trusting(&[&k])).is_ok());
    }

    #[test]
    fn versions_order_as_releases_do() {
        let ordered = [
            "v0.1.9",
            "v0.1.10",
            "v0.1.12",
            "v0.2.0",
            "v1.0.0-1",
            "v1.0.0-alpha",
            "v1.0.0-alpha.1",
            "v1.0.0-alpha.beta",
            "v1.0.0-beta.2",
            "v1.0.0-beta.11",
            "v1.0.0-rc.1",
            "v1.0.0",
            "v1.0.1",
            "v10.0.0",
        ];
        let parsed: Vec<_> = ordered
            .iter()
            .map(|v| ReleaseVersion::parse(v).unwrap_or_else(|_| panic!("{v} 應解得開")))
            .collect();
        for pair in parsed.windows(2) {
            assert!(pair[0] < pair[1], "{} 應低於 {}", pair[0], pair[1]);
        }
        for (text, version) in ordered.iter().zip(&parsed) {
            assert_eq!(&version.to_string(), text, "顯示應與原字串相同");
        }
        assert!(parsed[4].is_prerelease());
        assert!(!parsed[11].is_prerelease());
    }

    #[test]
    fn strings_that_are_not_release_versions_do_not_parse() {
        for text in [
            "",
            "v",
            "dev",
            "0.1.12",
            "V0.1.12",
            " v0.1.12",
            "v0.1",
            "v0.1.12.1",
            "v01.1.12",
            "v0.01.12",
            "v0.1.012",
            "v0.1.12-",
            "v0.1.12-rc.",
            "v0.1.12-rc..1",
            "v0.1.12-rc.01",
            "v0.1.12-rc_1",
            "v0.1.12+build.5",
            "v0.1.-1",
            "v99999999999999999999.0.0",
        ] {
            assert_eq!(ReleaseVersion::parse(text), Err(VersionError), "{text:?}");
        }
    }

    #[test]
    fn only_a_later_release_is_installed() {
        let k = key(7);
        let manifest = verify_with(&signed(&body("v0.1.13"), &[&k]), &trusting(&[&k])).unwrap();

        assert_eq!(manifest.decide("v0.1.12"), Decision::Install);
        assert_eq!(manifest.decide("v0.1.13-rc.1"), Decision::Install);
        assert_eq!(manifest.decide("v0.1.13"), Decision::UpToDate);
        assert_eq!(manifest.decide("v0.1.13\n"), Decision::UpToDate);

        // Replaying an older manifest has no effect beyond a recorded reason.
        let Decision::Ignore(reason) = manifest.decide("v0.2.0") else {
            panic!("較舊的版本清單不得安裝");
        };
        assert_eq!(
            reason,
            Refusal::Downgrade {
                installed: ReleaseVersion::parse("v0.2.0").unwrap(),
                offered: ReleaseVersion::parse("v0.1.13").unwrap(),
            }
        );
        assert!(reason.to_string().contains("v0.1.13"));
        assert!(reason.to_string().contains("v0.2.0"));

        // An installed version nobody can read is a refusal, not "older".
        for installed in ["", "dev", "0.1.12", "latest"] {
            assert_eq!(
                manifest.decide(installed),
                Decision::Ignore(Refusal::UnreadableInstalled {
                    installed: installed.into()
                }),
                "{installed:?}"
            );
        }
    }

    #[test]
    fn image_digests_are_checked_against_what_the_host_reports() {
        let k = key(7);
        let manifest = verify_with(&signed(&body("v0.1.13"), &[&k]), &trusting(&[&k])).unwrap();

        assert_eq!(manifest.check_image(Image::Api, API), Ok(()));
        assert_eq!(
            manifest.check_image(Image::Web, &format!("{WEB}\n")),
            Ok(())
        );

        let upper = API.to_uppercase();
        for reported in [WEB, "", "sha256:", upper.as_str(), "latest"] {
            let err = manifest
                .check_image(Image::Api, reported)
                .expect_err("不相符的映像不得啟動");
            assert_eq!(err.image, Image::Api);
            assert_eq!(err.expected.to_string(), API);
            assert!(err.to_string().contains("api"));
        }
    }

    #[test]
    fn image_names_round_trip() {
        for image in Image::ALL {
            assert_eq!(Image::from_name(image.name()), Some(image));
        }
        assert_eq!(Image::from_name("mcp"), None);
        assert_eq!(Image::from_name("API"), None);
    }

    /// A licence token signs its base64url payload text. Even with one key in
    /// both roles, neither kind of signed message can pass as the other.
    #[test]
    fn a_licence_token_signature_cannot_pass_as_a_manifest() {
        let k = key(7);
        let id = KeyId::of(&k.verifying_key());
        let trusted = trusting(&[&k]);

        // base64url of {"license_no":"L-2609-0001"}, the signed half of a token.
        let token_body = "eyJsaWNlbnNlX25vIjoiTC0yNjA5LTAwMDEifQ";
        let token_sig = hex(&k.sign(token_body.as_bytes()).to_bytes());
        let presented = format!("{token_body}\nsignature {id} {token_sig}\n");
        assert_eq!(
            verify_with(&presented, &trusted),
            Err(ManifestError::Malformed { line: 1 })
        );

        // The other way: every manifest body has bytes outside the base64url
        // alphabet, so no manifest signature is over a token's signed half.
        let b = body("v0.1.13");
        let base64url = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
        assert!(HEADER.bytes().any(|c| !base64url(c)));
        assert!(b.starts_with(HEADER));
    }

    #[test]
    fn the_compiled_in_keys_parse_and_are_distinct() {
        assert!(!TRUSTED_KEYS.is_empty());
        let keys = trusted_keys();
        assert_eq!(keys.len(), TRUSTED_KEYS.len(), "每一把公鑰都必須解得開");
        let mut ids: Vec<_> = keys.iter().map(KeyId::of).collect();
        ids.sort_by_key(|id| id.0);
        ids.dedup();
        assert_eq!(ids.len(), keys.len(), "金鑰識別碼不得重複");
        for hex in TRUSTED_KEYS {
            assert_eq!(hex.len(), 64);
            assert_eq!(
                parse_public_key(hex).map(|k| hex_of(&k)),
                Some(hex.to_string())
            );
        }
    }

    fn hex_of(key: &VerifyingKey) -> String {
        hex(key.as_bytes())
    }

    /// The same values scripts/release-manifest.ts derives; the fixture test
    /// below checks the two sides sign alike.
    #[test]
    fn key_ids_are_the_sha256_prefix_of_the_public_key() {
        let k = key(7).verifying_key();
        assert_eq!(
            hex_of(&k),
            "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
        );
        assert_eq!(KeyId::of(&k).to_string(), "fe812c12f3ab4ce6");
    }

    /// Written by `scripts/release-manifest.ts sign` with the seed 0x07 * 32;
    /// scripts/release-manifest-test.ts regenerates it and expects the same
    /// bytes, since Ed25519 signatures are deterministic.
    #[test]
    fn the_manifest_the_signing_script_writes_verifies() {
        let fixture = include_str!("../testdata/release-manifest.txt");
        let m = verify_with(fixture, &trusting(&[&key(7)])).expect("腳本簽出的清單應驗得過");
        assert_eq!(m.version().to_string(), "v0.1.13");
        assert_eq!(m.digest(Image::Api).to_string(), API);
        assert_eq!(m.digest(Image::Web).to_string(), WEB);
        assert_eq!(m.digest(Image::Backup).to_string(), BACKUP);
        assert_eq!(
            verify(fixture),
            Err(ManifestError::NoTrustedSignature),
            "測試金鑰不得是編進來的金鑰"
        );
    }
}
