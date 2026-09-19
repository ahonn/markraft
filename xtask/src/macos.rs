use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use plist::{Dictionary, Value};

use crate::{output, run};

const APP_NAME: &str = "Markraft.app";
const FEED_URL: &str = "https://github.com/ahonn/markraft/releases/latest/download/appcast.xml";

#[derive(Default)]
pub struct BundleOptions {
    pub release: bool,
    pub universal: bool,
    pub mock_updates: bool,
}

fn cargo(root: &Path) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env("SPARKLE_FRAMEWORK_PATH", root.join("target/sparkle"))
        .env("MACOSX_DEPLOYMENT_TARGET", "13.0");
    command
}

fn build_options(command: &mut Command, options: &BundleOptions, target: Option<&str>) {
    command.args(["-p", "markraft-app"]);
    if options.release {
        command.arg("--release");
    } else {
        command.args(["--profile", "dev"]);
    }
    if options.mock_updates {
        command.args(["--features", "updater-mock"]);
    }
    if let Some(target) = target {
        command.args(["--target", target]);
    }
}

fn icon(root: &Path) -> Result<()> {
    let directory = root.join("target/Markraft.iconset");
    fs::create_dir_all(&directory)?;
    for size in [16, 32, 128, 256, 512] {
        for scale in [1, 2] {
            let pixels = (size * scale).to_string();
            let suffix = if scale == 2 { "@2x" } else { "" };
            run(Command::new("sips")
                .args(["--resampleHeightWidth", &pixels, &pixels])
                .arg(root.join("assets/icon/Markraft.png"))
                .arg("--out")
                .arg(directory.join(format!("icon_{size}x{size}{suffix}.png"))))?;
        }
    }
    run(Command::new("iconutil")
        .args(["-c", "icns"])
        .arg(directory)
        .arg("-o")
        .arg(root.join("target/Markraft.icns")))
}

pub fn bundle(root: &Path, options: BundleOptions) -> Result<PathBuf> {
    ensure!(cfg!(target_os = "macos"), "App bundling requires macOS");
    let version = output(cargo(root).args(["bundle", "--version"]))?;
    ensure!(
        version.trim() == "cargo-bundle v0.11.0",
        "Install cargo-bundle: cargo install cargo-bundle --version 0.11.0 --locked"
    );
    run(Command::new("bash")
        .arg(root.join("scripts/download-sparkle.sh"))
        .current_dir(root))?;
    icon(root)?;

    let profile = if options.release { "release" } else { "debug" };
    let targets: &[Option<&str>] = if options.universal {
        &[Some("aarch64-apple-darwin"), Some("x86_64-apple-darwin")]
    } else {
        &[None]
    };
    for target in targets {
        let mut command = cargo(root);
        command.args(["build", "--locked"]);
        build_options(&mut command, &options, *target);
        run(&mut command)?;
    }

    let mut command = cargo(root);
    command
        .args(["bundle", "--format", "osx"])
        .env("CARGO_BUNDLE_SKIP_BUILD", "1");
    build_options(&mut command, &options, targets[0]);
    run(&mut command)?;

    let app = if options.universal {
        let app = root.join(format!(
            "target/universal-apple-darwin/{profile}/bundle/osx/{APP_NAME}"
        ));
        if app.exists() {
            fs::remove_dir_all(&app).context("Remove the previous universal app bundle")?;
        }
        run(Command::new("ditto")
            .arg(root.join(format!(
                "target/aarch64-apple-darwin/{profile}/bundle/osx/{APP_NAME}"
            )))
            .arg(&app))?;
        run(Command::new("lipo")
            .arg("-create")
            .arg(root.join(format!(
                "target/aarch64-apple-darwin/{profile}/markraft-app"
            )))
            .arg(root.join(format!("target/x86_64-apple-darwin/{profile}/markraft-app")))
            .arg("-output")
            .arg(app.join("Contents/MacOS/markraft-app")))?;
        app
    } else {
        root.join(format!("target/{profile}/bundle/osx/{APP_NAME}"))
    };
    let public_key = env::var("SPARKLE_PUBLIC_KEY").unwrap_or_default();
    configure(&app, &public_key, options.mock_updates)?;
    let identity = env::var("MARKRAFT_SIGN_IDENTITY").unwrap_or_else(|_| "-".into());
    sign(&app, &identity)?;
    run(Command::new("plutil")
        .arg("-lint")
        .arg(app.join("Contents/Info.plist")))?;
    Ok(app)
}

fn configure_metadata(info: &mut Dictionary, public_key: &str, mock: bool) -> Result<()> {
    let version = info
        .get("CFBundleShortVersionString")
        .and_then(Value::as_string)
        .context("App Info.plist must contain CFBundleShortVersionString")?
        .to_owned();
    crate::stable_version(&version)?;
    if !public_key.is_empty() {
        crate::crypto::validate_public_key(public_key)?;
    }
    // cargo-bundle generates a timestamp; Sparkle must compare release versions.
    info.insert("CFBundleVersion".into(), version.into());
    info.insert("LSUIElement".into(), true.into());
    info.insert("SUEnableAutomaticChecks".into(), (!mock).into());
    info.insert("SUAllowsAutomaticUpdates".into(), false.into());
    info.insert("SUAutomaticallyUpdate".into(), false.into());
    if public_key.is_empty() {
        info.remove("SUPublicEDKey");
    } else {
        info.insert("SUPublicEDKey".into(), public_key.into());
    }
    if mock {
        info.insert("MarkraftMockUpdates".into(), true.into());
        info.insert(
            "CFBundleIdentifier".into(),
            "dev.markraft.update-test".into(),
        );
        info.insert("CFBundleName".into(), "Markraft Update Test".into());
        info.insert("CFBundleDisplayName".into(), "Markraft Update Test".into());
        info.insert(
            "SUFeedURL".into(),
            "http://127.0.0.1:8765/appcast.xml".into(),
        );
        let mut transport = Dictionary::new();
        transport.insert("NSAllowsLocalNetworking".into(), true.into());
        info.insert("NSAppTransportSecurity".into(), transport.into());
    } else {
        info.insert("CFBundleIdentifier".into(), "dev.markraft.app".into());
        info.insert("CFBundleName".into(), "Markraft".into());
        info.insert("CFBundleDisplayName".into(), "Markraft".into());
        info.insert("SUFeedURL".into(), FEED_URL.into());
        info.remove("MarkraftMockUpdates");
        if let Some(Value::Dictionary(transport)) = info.get_mut("NSAppTransportSecurity") {
            transport.remove("NSAllowsLocalNetworking");
            if transport.is_empty() {
                info.remove("NSAppTransportSecurity");
            }
        }
    }
    Ok(())
}

pub fn configure(app: &Path, public_key: &str, mock: bool) -> Result<()> {
    let path = app.join("Contents/Info.plist");
    let mut info = Value::from_file(&path)
        .with_context(|| format!("Read {}", path.display()))?
        .into_dictionary()
        .context("App Info.plist must be a dictionary")?;
    configure_metadata(&mut info, public_key, mock)?;
    Value::Dictionary(info).to_file_xml(&path)?;
    Ok(())
}

pub fn sign(app: &Path, identity: &str) -> Result<()> {
    ensure!(!identity.is_empty(), "Signing identity must not be empty");
    let framework = app.join("Contents/Frameworks/Sparkle.framework");
    // Sign nested code inside-out, retaining the XPC services' sandbox entitlements.
    for (path, preserve_entitlements) in [
        (
            framework.join("Versions/B/XPCServices/Downloader.xpc"),
            true,
        ),
        (framework.join("Versions/B/XPCServices/Installer.xpc"), true),
        (framework.join("Versions/B/Autoupdate"), false),
        (framework.join("Versions/B/Updater.app"), false),
        (framework, false),
        (app.to_owned(), false),
    ] {
        let mut command = Command::new("codesign");
        command.args(["--force", "--sign", identity]);
        if identity != "-" {
            command.args(["--options", "runtime", "--timestamp"]);
        }
        if preserve_entitlements {
            command.arg("--preserve-metadata=entitlements");
        }
        run(command.arg(path))?;
    }
    run(Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app))
}

pub fn zip(app: &Path, archive: &Path) -> Result<()> {
    if let Some(parent) = archive.parent() {
        fs::create_dir_all(parent)?;
    }
    run(Command::new("ditto")
        .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
        .arg(app)
        .arg(archive))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    fn public_key() -> String {
        STANDARD.encode(
            ed25519_dalek::SigningKey::from_bytes(&[7; 32])
                .verifying_key()
                .as_bytes(),
        )
    }

    fn metadata(version: &str) -> Dictionary {
        let mut info = Dictionary::new();
        info.insert("CFBundleShortVersionString".into(), version.into());
        info.insert("CFBundleVersion".into(), "timestamp".into());
        info
    }

    #[test]
    fn release_metadata_uses_comparable_versions_and_user_initiated_installation() {
        let key = public_key();
        let mut info = metadata("0.2.1");
        configure_metadata(&mut info, &key, false).unwrap();
        assert_eq!(info["CFBundleVersion"].as_string(), Some("0.2.1"));
        assert_eq!(info["SUPublicEDKey"].as_string(), Some(key.as_str()));
        assert_eq!(info["SUFeedURL"].as_string(), Some(FEED_URL));
        assert_eq!(info["LSUIElement"].as_boolean(), Some(true));
        assert_eq!(info["SUEnableAutomaticChecks"].as_boolean(), Some(true));
        assert_eq!(info["SUAllowsAutomaticUpdates"].as_boolean(), Some(false));
        assert_eq!(info["SUAutomaticallyUpdate"].as_boolean(), Some(false));
    }

    #[test]
    fn switching_to_production_removes_mock_identity_transport_and_stale_key() {
        let mut info = metadata("0.1.0");
        configure_metadata(&mut info, &public_key(), true).unwrap();
        assert_eq!(info["MarkraftMockUpdates"].as_boolean(), Some(true));
        assert_eq!(
            info["CFBundleIdentifier"].as_string(),
            Some("dev.markraft.update-test")
        );
        assert_eq!(
            info["SUFeedURL"].as_string(),
            Some("http://127.0.0.1:8765/appcast.xml")
        );
        assert_eq!(info["SUEnableAutomaticChecks"].as_boolean(), Some(false));
        configure_metadata(&mut info, "", false).unwrap();
        assert_eq!(
            info["CFBundleIdentifier"].as_string(),
            Some("dev.markraft.app")
        );
        assert!(!info.contains_key("MarkraftMockUpdates"));
        assert!(!info.contains_key("NSAppTransportSecurity"));
        assert!(!info.contains_key("SUPublicEDKey"));
    }

    #[test]
    fn rejected_metadata_does_not_mutate_the_bundle() {
        for version in ["0.1.0-beta.1", "v1.0.0", "1.2", "01.0.0"] {
            let mut info = metadata(version);
            let original = info.clone();
            assert!(configure_metadata(&mut info, "", false).is_err());
            assert_eq!(info, original);
        }
        for key in ["not-base64".to_owned(), STANDARD.encode([7; 31])] {
            let mut info = metadata("0.1.0");
            let original = info.clone();
            assert!(configure_metadata(&mut info, &key, false).is_err());
            assert_eq!(info, original);
        }
    }
}
