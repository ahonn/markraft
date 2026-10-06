//! Build phase for the native Xcode application target. Xcode owns the final
//! Info.plist, provisioning profile, code signature, and archive structure.

use std::{env, fs, path::Path, process::Command};

use anyhow::{Context, Result, bail, ensure};
use plist::{Dictionary, Value};

use crate::{macos, output, run};

fn required(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .with_context(|| {
            format!("Xcode must provide {name}; run this task through Markraft.xcodeproj")
        })
}

fn targets(architectures: &str) -> Result<Vec<&'static str>> {
    let mut targets = Vec::new();
    for architecture in architectures.split_whitespace() {
        let target = match architecture {
            "arm64" => "aarch64-apple-darwin",
            "x86_64" => "x86_64-apple-darwin",
            _ => bail!("Unsupported Xcode architecture: {architecture}"),
        };
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    ensure!(!targets.is_empty(), "Xcode ARCHS must not be empty");
    Ok(targets)
}

fn build_number(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && !value.starts_with('0')
            && value.bytes().all(|byte| byte.is_ascii_digit()),
        "CURRENT_PROJECT_VERSION must be a positive integer, got {value}"
    );
    Ok(())
}

fn configure_metadata(info: &mut Dictionary, number: &str, identifier: &str) -> Result<()> {
    build_number(number)?;
    ensure!(
        info.get("CFBundleIdentifier").and_then(Value::as_string) == Some(identifier),
        "Xcode PRODUCT_BUNDLE_IDENTIFIER does not match the store bundle"
    );
    info.insert("CFBundleVersion".into(), number.into());
    Ok(())
}

fn compile_command(root: &Path, target: &str, profile: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target/xcode-cargo"))
        .env("MACOSX_DEPLOYMENT_TARGET", "13.0")
        .args([
            "build",
            "--locked",
            "--package",
            "markraft-app",
            "--no-default-features",
            "--features",
            "mac-app-store",
            "--profile",
            profile,
            "--target",
            target,
        ]);
    command
}

pub fn build(root: &Path) -> Result<()> {
    ensure!(required("PLATFORM_NAME")? == "macosx", "Expected macOS");
    let profile = match required("CONFIGURATION")?.as_str() {
        "Release" => "app-store",
        "Debug" => "dev",
        other => bail!("Unsupported Xcode configuration: {other}"),
    };
    let targets = targets(&required("ARCHS")?)?;
    let number = required("CURRENT_PROJECT_VERSION")?;
    build_number(&number)?;
    let identifier = required("PRODUCT_BUNDLE_IDENTIFIER")?;
    // The Cloud hook will supply CURRENT_PROJECT_VERSION before xcodebuild.
    // Reject a stale local default rather than silently uploading build 1.
    if env::var("CI_XCODE_CLOUD").as_deref() == Ok("TRUE") {
        ensure!(
            required("CI_BUILD_NUMBER")? == number,
            "CURRENT_PROJECT_VERSION must match CI_BUILD_NUMBER"
        );
    }
    let target_directory = required("TARGET_BUILD_DIR")?;
    let target_directory = Path::new(&target_directory);
    let executable = target_directory.join(required("EXECUTABLE_PATH")?);
    let resources = target_directory.join(required("UNLOCALIZED_RESOURCES_FOLDER_PATH")?);
    let input_plist = required("INFOPLIST_FILE")?;
    let input_plist = Path::new(&input_plist);
    let dsym =
        Path::new(&required("DWARF_DSYM_FOLDER_PATH")?).join(required("DWARF_DSYM_FILE_NAME")?);

    let mut binaries = Vec::new();
    for target in targets {
        run(&mut compile_command(root, target, profile))?;
        let directory = if profile == "dev" { "debug" } else { profile };
        binaries.push(
            root.join("target/xcode-cargo")
                .join(target)
                .join(directory)
                .join("markraft-app"),
        );
    }

    let stage = tempfile::tempdir_in(root.join("target"))?;
    let app = stage.path().join("Markraft.app");
    macos::assemble_store(root, &binaries, &app)?;
    let mut info = Value::from_file(app.join("Contents/Info.plist"))?
        .into_dictionary()
        .context("Store Info.plist must contain a dictionary")?;
    configure_metadata(&mut info, &number, &identifier)?;

    fs::create_dir_all(executable.parent().context("Executable needs a parent")?)?;
    fs::create_dir_all(&resources)?;
    fs::create_dir_all(input_plist.parent().context("Info.plist needs a parent")?)?;
    fs::create_dir_all(dsym.parent().context("dSYM needs a parent")?)?;
    fs::copy(app.join("Contents/MacOS/markraft-app"), &executable)?;
    for entry in fs::read_dir(app.join("Contents/Resources"))? {
        let entry = entry?;
        fs::copy(entry.path(), resources.join(entry.file_name()))?;
    }
    // This is a generated input, not the Info.plist inside the final app.
    Value::Dictionary(info).to_file_xml(input_plist)?;
    run(Command::new("xcrun")
        .arg("dsymutil")
        .arg(&executable)
        .arg("-o")
        .arg(&dsym))?;
    let binary_uuids = output(
        Command::new("xcrun")
            .args(["dwarfdump", "--uuid"])
            .arg(&executable),
    )?;
    let symbol_uuids = output(
        Command::new("xcrun")
            .args(["dwarfdump", "--uuid"])
            .arg(&dsym),
    )?;
    let uuids = |text: &str| -> Vec<String> {
        text.lines()
            .map(|line| {
                line.split_whitespace()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    ensure!(
        !binary_uuids.is_empty() && uuids(&binary_uuids) == uuids(&symbol_uuids),
        "Rust executable and dSYM UUIDs must match"
    );
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn xcode_architectures_select_only_requested_rust_targets() {
        assert_eq!(targets("x86_64").unwrap(), ["x86_64-apple-darwin"]);
        assert_eq!(
            targets("arm64 x86_64 arm64").unwrap(),
            ["aarch64-apple-darwin", "x86_64-apple-darwin"]
        );
        for unsupported in ["", " ", "arm64e", "arm64 i386"] {
            assert!(targets(unsupported).is_err());
        }
    }

    #[test]
    fn archive_number_is_independent_of_the_marketing_version() {
        let mut info = Dictionary::new();
        info.insert("CFBundleIdentifier".into(), "app.markraft.mac".into());
        info.insert("CFBundleShortVersionString".into(), "0.1.7".into());
        info.insert("CFBundleVersion".into(), "0.1.7".into());
        configure_metadata(&mut info, "42", "app.markraft.mac").unwrap();
        assert_eq!(info["CFBundleVersion"].as_string(), Some("42"));
        assert_eq!(
            info["CFBundleShortVersionString"].as_string(),
            Some("0.1.7")
        );
        for invalid in ["", "0", "01", "-1", "1.2", " 2", "2\n"] {
            let original = info.clone();
            assert!(configure_metadata(&mut info, invalid, "app.markraft.mac").is_err());
            assert_eq!(info, original);
        }
        assert!(configure_metadata(&mut info, "42", "app.markraft.storetest").is_err());
    }

    #[test]
    fn xcode_compilation_is_isolated_from_direct_distribution() {
        let command = compile_command(Path::new("/repo"), "x86_64-apple-darwin", "app-store");
        let args: Vec<_> = command
            .get_args()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert!(args.contains(&"--locked"));
        assert!(
            args.windows(3)
                .any(|part| part == ["--no-default-features", "--features", "mac-app-store"])
        );
        assert!(
            args.windows(2)
                .any(|part| part == ["--profile", "app-store"])
        );
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "CARGO_TARGET_DIR"
                    && value == Some(std::ffi::OsStr::new("/repo/target/xcode-cargo")))
        );
    }
}
