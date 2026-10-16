//! Release manifests on this server: which signed release each licence is
//! offered.
//!
//! Laysi staff upload the `release-manifest.txt` attached to a GitHub release.
//! A licence is offered at most one of them, through `license.release_version`,
//! and a successful check carries that manifest's text unchanged. This server
//! cannot produce a manifest: the crate it shares with the product only
//! verifies them, and the signing key is a CI secret that never comes here.
//!
//! Offering is all a release does. The installation's system administrator
//! decides when to install it, and the installation refuses anything older
//! than what it runs.

use laysi_license_token::release::{
    self, ManifestError, ReleaseManifest, ReleaseVersion, VerifyingKey, MAX_MANIFEST_BYTES,
};

/// What every upload refusal tells the reader to do, since the fix for a file
/// that does not verify is always the file on the release itself.
const FETCH_AGAIN: &str = "請從 GitHub release 重新下載 release-manifest.txt 直接上傳。";

/// An uploaded manifest that verified: its version, and its text exactly as
/// it arrived.
#[derive(Debug)]
pub struct Accepted {
    pub version: ReleaseVersion,
    pub text: String,
}

/// Check an upload before it is stored. The `Err` is the sentence the console
/// prints.
///
/// Carriage returns are named before the verifier sees them: a file opened and
/// saved on Windows is the likeliest way to get here, and "line 1 is
/// malformed" would not say so. Line endings are never rewritten, because the
/// signature covers the bytes as they were signed.
pub fn accept(bytes: &[u8], trusted: &[VerifyingKey]) -> Result<Accepted, String> {
    if bytes.is_empty() {
        return Err("請選擇 release-manifest.txt 再上傳。".into());
    }
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(too_large());
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(format!("檔案內容不是版本清單。{FETCH_AGAIN}"));
    };
    if text.contains('\r') {
        return Err(format!(
            "檔案的換行已被改成 Windows 格式（CRLF）。{FETCH_AGAIN}"
        ));
    }
    match release::verify_with(text, trusted) {
        Ok(manifest) => Ok(Accepted {
            version: manifest.version().clone(),
            text: text.to_string(),
        }),
        Err(error) => Err(refusal(&error)),
    }
}

/// The refusal for a file over [`MAX_MANIFEST_BYTES`], also used when the
/// request itself is cut off before the file is read.
pub fn too_large() -> String {
    format!("檔案超過 {MAX_MANIFEST_BYTES} 位元組。{FETCH_AGAIN}")
}

/// A verifier error in words staff can act on. Key ids stay off the screen:
/// nobody uploading a file can do anything with one.
fn refusal(error: &ManifestError) -> String {
    match error {
        ManifestError::TooLarge { .. } => too_large(),
        ManifestError::Malformed { line } => format!("第 {line} 行格式不符。{FETCH_AGAIN}"),
        ManifestError::UnsupportedFormat => {
            "版本清單的格式比本伺服器新，請先更新授權伺服器再上傳。".into()
        }
        ManifestError::Version => format!("版本號格式不符。{FETCH_AGAIN}"),
        ManifestError::Digest { image } => format!("{image} 映像的 digest 格式不符。{FETCH_AGAIN}"),
        ManifestError::Unsigned => format!("版本清單沒有簽章。{FETCH_AGAIN}"),
        ManifestError::DuplicateKeyId(_) => format!("同一把金鑰的簽章出現兩次。{FETCH_AGAIN}"),
        ManifestError::NoTrustedSignature => {
            "版本清單沒有本伺服器信任的簽章。請確認檔案取自 GitHub release；發布金鑰更換過時，請先更新授權伺服器。".into()
        }
        ManifestError::BadSignature(_) => format!("版本清單的簽章與內容不符。{FETCH_AGAIN}"),
    }
}

/// What storing an upload did. A version is built once and has one manifest,
/// so the same bytes again change nothing and other bytes are refused rather
/// than replacing the ones licences may already be offered.
#[derive(Debug, PartialEq, Eq)]
pub enum Stored {
    New,
    Same,
    Conflict,
}

/// Compare an upload with what is already stored under its version.
pub fn compare(existing: Option<&str>, uploaded: &str) -> Stored {
    match existing {
        None => Stored::New,
        Some(text) if text == uploaded => Stored::Same,
        Some(_) => Stored::Conflict,
    }
}

/// A stored manifest, if it still verifies against the keys this build
/// trusts.
pub fn trusted(text: &str, keys: &[VerifyingKey]) -> Option<ReleaseManifest> {
    release::verify_with(text, keys).ok()
}

/// Why `offered` may not be released to a licence whose installation last
/// reported `installed`, or `None` when it may.
///
/// The installation ignores anything older than what it runs, so releasing it
/// would change nothing on that machine and say otherwise on this screen. A
/// report that is not a release version (never checked in, or a build from a
/// working tree) cannot be compared and does not block.
pub fn downgrade(license_no: &str, installed: &str, offered: &ReleaseVersion) -> Option<String> {
    let current = ReleaseVersion::parse(installed.trim()).ok()?;
    (current > *offered)
        .then(|| format!("{license_no} 已安裝 {current}，請放行 {current} 之後的版本。"))
}

/// The manifest a successful check carries, or `None`.
///
/// Verified again on the way out, against the keys of the build that is
/// sending it. A manifest uploaded under a key a later build of this server
/// no longer trusts is withheld rather than forwarded: dropping a key from
/// `TRUSTED_KEYS` is how Laysi stops vouching for what it signed, and the
/// installations that dropped it too would refuse the manifest anyway.
pub fn forward(license_no: &str, stored: Option<String>, keys: &[VerifyingKey]) -> Option<String> {
    let text = stored?;
    match release::verify_with(&text, keys) {
        Ok(_) => Some(text),
        Err(error) => {
            tracing::warn!(
                license = %license_no,
                %error,
                "released manifest no longer verifies; withheld from the check answer"
            );
            None
        }
    }
}

#[cfg(test)]
mod database;

#[cfg(test)]
pub mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use laysi_license_token::release::KeyId;

    pub const API: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    pub const WEB: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    pub const BACKUP: &str =
        "sha256:3333333333333333333333333333333333333333333333333333333333333333";

    /// A release signing key for tests. Never one of `TRUSTED_KEYS`.
    pub fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    /// A manifest for `version` signed by `signer`, in the bytes the release
    /// workflow writes.
    pub fn manifest(version: &str, signer: &SigningKey) -> String {
        let body = format!(
            "laysi-release-manifest 1\nversion {version}\nimage api {API}\nimage web {WEB}\nimage backup {BACKUP}\n"
        );
        let sig: String = signer
            .sign(body.as_bytes())
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!(
            "{body}signature {} {sig}\n",
            KeyId::of(&signer.verifying_key())
        )
    }

    fn trusting(signer: &SigningKey) -> Vec<VerifyingKey> {
        vec![signer.verifying_key()]
    }

    #[test]
    fn a_signed_manifest_is_accepted_byte_for_byte() {
        let k = key(7);
        let text = manifest("v0.1.13", &k);
        let accepted = accept(text.as_bytes(), &trusting(&k)).expect("應接受");
        assert_eq!(accepted.version.to_string(), "v0.1.13");
        assert_eq!(accepted.text, text, "存下來的必須是上傳的原始位元組");
    }

    #[test]
    fn a_manifest_signed_by_an_untrusted_key_is_refused() {
        let text = manifest("v0.1.13", &key(8));
        let refused = accept(text.as_bytes(), &trusting(&key(7))).unwrap_err();
        assert!(refused.contains("信任的簽章"), "{refused}");

        // The compiled-in keys are not the test keys either, which is what
        // keeps a test manifest from ever passing in production.
        let refused = accept(text.as_bytes(), &release::trusted_keys()).unwrap_err();
        assert!(refused.contains("信任的簽章"), "{refused}");
    }

    #[test]
    fn an_edited_manifest_is_refused_without_naming_the_key() {
        let k = key(7);
        let edited = manifest("v0.1.13", &k).replace("version v0.1.13", "version v0.1.14");
        let refused = accept(edited.as_bytes(), &trusting(&k)).unwrap_err();
        assert!(refused.contains("簽章與內容不符"), "{refused}");
        let id = KeyId::of(&k.verifying_key()).to_string();
        assert!(!refused.contains(&id), "畫面上不出現金鑰識別碼：{refused}");
        assert!(refused.contains("release-manifest.txt"), "要說出怎麼修");
    }

    #[test]
    fn an_oversize_upload_is_refused_before_it_is_read_as_text() {
        let k = key(7);
        let mut text = manifest("v0.1.13", &k);
        while text.len() <= MAX_MANIFEST_BYTES {
            text.push('x');
        }
        let refused = accept(text.as_bytes(), &trusting(&k)).unwrap_err();
        assert!(
            refused.contains(&MAX_MANIFEST_BYTES.to_string()),
            "{refused}"
        );

        // Not text at all, and over the limit: the size is what is reported.
        let binary = vec![0xffu8; MAX_MANIFEST_BYTES + 1];
        assert_eq!(accept(&binary, &trusting(&k)).unwrap_err(), too_large());
    }

    #[test]
    fn crlf_line_endings_are_refused_rather_than_repaired() {
        let k = key(7);
        let text = manifest("v0.1.13", &k).replace('\n', "\r\n");
        let refused = accept(text.as_bytes(), &trusting(&k)).unwrap_err();
        assert!(refused.contains("CRLF"), "{refused}");
        // Repairing it would verify, which is exactly why it is not done: the
        // stored text has to be the bytes that were signed.
        assert!(accept(text.replace("\r\n", "\n").as_bytes(), &trusting(&k)).is_ok());
    }

    #[test]
    fn empty_and_non_text_uploads_are_refused() {
        let k = key(7);
        assert!(accept(b"", &trusting(&k)).unwrap_err().contains("請選擇"));
        assert!(accept(&[0xff, 0xfe, 0x00], &trusting(&k))
            .unwrap_err()
            .contains("不是版本清單"));
    }

    #[test]
    fn one_version_keeps_one_manifest() {
        let (a, b) = (key(7), key(8));
        let first = manifest("v0.1.13", &a);
        let other = manifest("v0.1.13", &b);
        assert_eq!(compare(None, &first), Stored::New);
        assert_eq!(compare(Some(&first), &first), Stored::Same);
        assert_eq!(compare(Some(&first), &other), Stored::Conflict);
    }

    #[test]
    fn a_release_older_than_the_installed_version_is_refused() {
        let offered = ReleaseVersion::parse("v0.1.13").unwrap();
        let refused = downgrade("L2610-001", "v0.1.14", &offered).expect("較舊的版本不得放行");
        assert!(refused.contains("L2610-001") && refused.contains("v0.1.14"));
        // The same version, or a later one, may be released.
        assert_eq!(downgrade("L2610-001", "v0.1.13", &offered), None);
        assert_eq!(downgrade("L2610-001", "v0.1.12", &offered), None);
        assert_eq!(downgrade("L2610-001", "v0.1.13-rc.1", &offered), None);
        // Nothing to compare against does not block.
        for installed in ["", "dev", "0.1.12"] {
            assert_eq!(
                downgrade("L2610-001", installed, &offered),
                None,
                "{installed:?}"
            );
        }
    }

    #[test]
    fn only_a_manifest_that_still_verifies_is_forwarded() {
        let (old, new) = (key(7), key(8));
        let text = manifest("v0.1.13", &old);
        assert_eq!(
            forward("L2610-001", Some(text.clone()), &trusting(&old)),
            Some(text.clone())
        );
        assert_eq!(forward("L2610-001", None, &trusting(&old)), None);
        // A build that no longer trusts the key withholds the manifest.
        assert_eq!(forward("L2610-001", Some(text), &trusting(&new)), None);
    }

    /// A file by its path from the repository root.
    fn repo_file(path: &str) -> String {
        let full = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(path);
        std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{}: {e}", full.display()))
    }

    #[test]
    fn the_licence_server_deploy_never_receives_the_release_manifest_signing_key() {
        const SECRET: &str = "RELEASE_MANIFEST_SIGNING_KEY";
        // The name the release workflow signs with. If it is renamed there,
        // this fails rather than going on to check a name nobody uses.
        assert!(
            repo_file(".github/workflows/release.yml").contains(&format!("secrets.{SECRET}")),
            "release.yml 已不以 {SECRET} 簽版本清單，請更新本測試"
        );

        let mut deploy_path = vec![
            ".github/workflows/license-release.yml".to_string(),
            ".github/workflows/license-deploy.yml".to_string(),
            "license-server/Dockerfile".to_string(),
        ];
        let mut compose: Vec<String> = std::fs::read_dir(env!("CARGO_MANIFEST_DIR"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("docker-compose") && name.ends_with(".yml"))
            .map(|name| format!("license-server/{name}"))
            .collect();
        assert!(
            !compose.is_empty(),
            "找不到 license-server/docker-compose*.yml"
        );
        compose.sort();
        deploy_path.extend(compose);

        for path in &deploy_path {
            assert!(
                !repo_file(path).contains(SECRET),
                "{path} 提到 {SECRET}：授權伺服器不得持有版本清單的簽章私鑰"
            );
        }

        // `inherit` would hand the deploy every repository secret, this one
        // included, without the name appearing in either file.
        for path in &deploy_path[..2] {
            let inherits = repo_file(path)
                .lines()
                .map(|line| line.split('#').next().unwrap_or("").trim())
                .any(|line| line == "secrets: inherit");
            assert!(
                !inherits,
                "{path} 不得使用 secrets: inherit，請逐一傳入需要的 secret"
            );
        }
    }
}
