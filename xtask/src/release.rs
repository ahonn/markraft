use crate::{crypto, macos, output, run, stable_version};
use anyhow::{Context, Result, ensure};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

pub fn generate_appcast(
    root: &Path,
    archives: &Path,
    key_file: &Path,
    download_prefix: &str,
    full_notes_url: &str,
) -> Result<PathBuf> {
    let feed = archives.join("appcast.xml");
    run(
        Command::new(root.join("target/sparkle/sparkle-bin/generate_appcast"))
            .arg("--ed-key-file")
            .arg(key_file)
            .args([
                "--download-url-prefix",
                download_prefix,
                "--full-release-notes-url",
                full_notes_url,
                "--maximum-deltas",
                "0",
                "--maximum-versions",
                "1",
            ])
            .arg("-o")
            .arg(&feed)
            .arg(archives),
    )?;
    ensure!(feed.is_file(), "Sparkle did not generate an appcast");
    Ok(feed)
}

pub fn verify_feed(feed: &Path, archive: &Path, public_key: &str) -> Result<()> {
    let xml = fs::read_to_string(feed)?;
    let document = roxmltree::Document::parse(&xml)?;
    let enclosures: Vec<_> = document
        .descendants()
        .filter(|n| n.has_tag_name("enclosure"))
        .collect();
    ensure!(
        enclosures.len() == 1,
        "Expected one full update in the generated appcast"
    );
    let enclosure = enclosures[0];
    let signature = enclosure
        .attribute((
            "http://www.andymatuschak.org/xml-namespaces/sparkle",
            "edSignature",
        ))
        .context("Appcast is missing the Ed25519 archive signature")?;
    let length: u64 = enclosure
        .attribute("length")
        .context("Appcast is missing the archive length")?
        .parse()?;
    ensure!(
        length == fs::metadata(archive)?.len(),
        "Appcast archive length does not match"
    );
    crypto::verify_archive(archive, public_key, signature)
}

fn required_env(name: &str) -> Result<String> {
    let value = env::var(name).with_context(|| format!("Set {name} before releasing"))?;
    ensure!(!value.is_empty(), "{name} must not be empty");
    Ok(value)
}

struct Notary {
    apple_id: String,
    team: String,
    password: String,
}

impl Notary {
    /// Notarize a ZIP or disk image, then staple the ticket to `target`.
    fn notarize(&self, submission: &Path, target: &Path) -> Result<()> {
        run(Command::new("xcrun")
            .args(["notarytool", "submit"])
            .arg(submission)
            .args([
                "--wait",
                "--apple-id",
                &self.apple_id,
                "--team-id",
                &self.team,
                "--password",
                &self.password,
            ]))?;
        run(Command::new("xcrun")
            .args(["stapler", "staple"])
            .arg(target))?;
        run(Command::new("xcrun")
            .args(["stapler", "validate"])
            .arg(target))
    }
}

pub fn release(root: &Path, tag: &str) -> Result<()> {
    let identity = required_env("MARKRAFT_SIGN_IDENTITY")?;
    ensure!(
        identity.starts_with("Developer ID Application:"),
        "Distribution requires a Developer ID Application identity"
    );
    let public_key = required_env("SPARKLE_PUBLIC_KEY")?;
    crypto::validate_public_key(&public_key)?;
    let key_file = PathBuf::from(required_env("SPARKLE_PRIVATE_KEY_FILE")?);
    ensure!(
        key_file.is_file(),
        "Sparkle private key file does not exist"
    );
    let notary = Notary {
        apple_id: required_env("APPLE_ID")?,
        team: required_env("APPLE_TEAM_ID")?,
        password: required_env("APPLE_APP_SPECIFIC_PASSWORD")?,
    };
    let metadata: serde_json::Value = serde_json::from_str(&output(
        Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
            .current_dir(root),
    )?)?;
    let version = metadata["packages"]
        .as_array()
        .context("Cargo metadata packages missing")?
        .iter()
        .find(|p| p["name"] == "markraft-app")
        .and_then(|p| p["version"].as_str())
        .context("App version missing")?;
    stable_version(version)?;
    ensure!(
        tag == format!("v{version}"),
        "Release tag must equal v{version}"
    );
    let app = macos::bundle(
        root,
        macos::BundleOptions {
            release: true,
            universal: true,
            mock_updates: false,
        },
    )?;
    // Keep the submission ZIP outside the appcast source directory. Only the
    // final stapled archive may be advertised and signed as an update.
    let temporary = tempfile::tempdir_in(root.join("target"))?;
    let submission = temporary.path().join("notarization.zip");
    macos::zip(&app, &submission)?;
    notary.notarize(&submission, &app)?;
    run(Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app))?;
    run(Command::new("spctl")
        .args(["--assess", "--type", "execute", "--verbose"])
        .arg(&app))?;
    let staging = temporary.path().join("artifacts");
    fs::create_dir(&staging)?;
    let filename = format!("Markraft-{version}-universal.zip");
    let archive = staging.join(&filename);
    macos::zip(&app, &archive)?;
    let feed = generate_appcast(
        root,
        &staging,
        &key_file,
        &format!("https://github.com/ahonn/markraft/releases/download/{tag}/"),
        &format!("https://github.com/ahonn/markraft/releases/tag/{tag}"),
    )?;
    verify_feed(&feed, &archive, &public_key)?;
    // The disk image is what people download; updates keep using the ZIP. It joins
    // the folder only now because generate_appcast advertises every archive it finds.
    let image_name = format!("Markraft-{version}-universal.dmg");
    let image = staging.join(&image_name);
    macos::dmg(&app, &image)?;
    macos::sign_image(&image, &identity)?;
    // The app inside is already stapled, so it opens offline once dragged out.
    notary.notarize(&image, &image)?;
    run(Command::new("spctl")
        .args(["--assess", "--type", "open"])
        .args(["--context", "context:primary-signature", "--verbose"])
        .arg(&image))?;
    let checksums = output(
        Command::new("shasum")
            .args(["-a", "256", &image_name, &filename, "appcast.xml"])
            .current_dir(&staging),
    )?;
    fs::write(staging.join("SHA256SUMS"), format!("{checksums}\n"))?;
    let destination = root.join("target/release-artifacts");
    fs::create_dir_all(&destination)?;
    for filename in [&image_name, &filename, "appcast.xml", "SHA256SUMS"] {
        fs::copy(staging.join(filename), destination.join(filename))?;
    }
    println!(
        "Release artifacts ready in {} for {tag}; nothing uploaded.",
        destination.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn validates_feed_signature_and_length_before_release() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("app.zip");
        fs::write(&archive, b"archive")?;
        let key = SigningKey::from_bytes(&[9; 32]);
        let public = STANDARD.encode(key.verifying_key().as_bytes());
        let signature = STANDARD.encode(key.sign(b"archive").to_bytes());
        let feed = dir.path().join("appcast.xml");
        for (length, valid) in [(7, true), (8, false)] {
            fs::write(
                &feed,
                format!(
                    r#"<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><item><enclosure length="{length}" sparkle:edSignature="{signature}"/></item></channel></rss>"#
                ),
            )?;
            assert_eq!(verify_feed(&feed, &archive, &public).is_ok(), valid);
        }
        Ok(())
    }

    #[test]
    fn refuses_a_feed_that_would_not_update_safely() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("app.zip");
        fs::write(&archive, b"archive")?;
        let key = SigningKey::from_bytes(&[9; 32]);
        let public = STANDARD.encode(key.verifying_key().as_bytes());
        let signed = STANDARD.encode(key.sign(b"archive").to_bytes());
        let forged = STANDARD.encode(key.sign(b"another archive").to_bytes());
        let enclosure = |signature: Option<&str>| match signature {
            Some(signature) => {
                format!(r#"<enclosure length="7" sparkle:edSignature="{signature}"/>"#)
            }
            None => r#"<enclosure length="7"/>"#.to_owned(),
        };
        let feed = dir.path().join("appcast.xml");
        for (items, reason) in [
            (enclosure(Some(&forged)), "signature"),
            (enclosure(None), "Ed25519 archive signature"),
            (String::new(), "one full update"),
            (enclosure(Some(&signed)).repeat(2), "one full update"),
        ] {
            fs::write(
                &feed,
                format!(
                    r#"<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><item>{items}</item></channel></rss>"#
                ),
            )?;
            let error = verify_feed(&feed, &archive, &public)
                .expect_err("the feed must be refused")
                .to_string();
            assert!(error.contains(reason), "{reason:?} not in {error:?}");
        }
        Ok(())
    }
}
