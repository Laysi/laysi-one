//! Verify a release manifest file the way an installation does.
//!
//!   cargo run -q -p laysi-license-token --example verify-release-manifest -- \
//!       <file> [--key <public key hex>]... [--version <expected version>]
//!
//! Without `--key` the keys compiled into this build are used, which is the
//! check release.yml runs before it attaches a manifest to a release. `--key`
//! replaces them, for the CI test that signs with throwaway keys. Prints the
//! version and digests and exits 0, or prints the reason and exits 1.
//!
//! The file is read through a limit of `MAX_MANIFEST_BYTES` plus one byte, the
//! same way anything reading a manifest from disk or the network should.

use std::io::Read;
use std::process::ExitCode;

use laysi_license_token::release::{self, MAX_MANIFEST_BYTES};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("{reason}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut file = None;
    let mut keys = Vec::new();
    let mut expected = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => {
                let hex = args.next().ok_or("--key needs a value")?;
                keys.push(release::parse_public_key(&hex).ok_or("--key is not a public key")?);
            }
            "--version" => expected = Some(args.next().ok_or("--version needs a value")?),
            _ if file.is_none() && !arg.starts_with("--") => file = Some(arg),
            _ => return Err(format!("unexpected argument {arg}")),
        }
    }
    let file =
        file.ok_or("usage: verify-release-manifest <file> [--key <hex>]... [--version <v>]")?;

    let mut bytes = Vec::new();
    std::fs::File::open(&file)
        .and_then(|f| {
            f.take(MAX_MANIFEST_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|e| format!("{file}: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{file}: not UTF-8"))?;

    let manifest = if keys.is_empty() {
        release::verify(&text)
    } else {
        release::verify_with(&text, &keys)
    }
    .map_err(|e| e.to_string())?;

    if let Some(expected) = expected {
        if manifest.version().to_string() != expected {
            return Err(format!(
                "manifest is for {}, expected {expected}",
                manifest.version()
            ));
        }
    }

    println!("version {}", manifest.version());
    for (image, digest) in manifest.digests() {
        println!("image {image} {digest}");
    }
    for id in manifest.signed_by() {
        println!("signed by {id}");
    }
    Ok(())
}
