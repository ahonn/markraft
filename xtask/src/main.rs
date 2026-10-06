#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
mod crypto;
mod macos;
mod mock;
mod mutants;
mod release;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::Command};

#[derive(Parser)]
#[command(about = "Build, release, and test Markraft macOS bundles")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Assemble and sign a local app bundle.
    Bundle {
        #[arg(long)]
        release: bool,
        #[arg(long)]
        universal: bool,
        #[arg(long)]
        mock_updates: bool,
        /// Build a sandboxed App Store bundle without the Sparkle updater.
        #[arg(long, conflicts_with_all = ["mock_updates", "dmg"])]
        mac_app_store: bool,
        /// Also package the bundle as a disk image, signed only with a Developer ID.
        #[arg(long)]
        dmg: bool,
        /// Bundle the binaries `compile` already built instead of building them.
        #[arg(long)]
        prebuilt: bool,
    },
    /// Build the release binary for one architecture of the universal app.
    Compile {
        #[arg(long)]
        target: String,
        #[arg(long)]
        mac_app_store: bool,
    },
    /// Build signed, notarized artifacts without uploading them.
    Release {
        tag: String,
        /// Bundle the binaries `compile` already built instead of building them.
        #[arg(long)]
        prebuilt: bool,
    },
    /// Prepare or serve isolated local update fixtures.
    Mock {
        #[command(subcommand)]
        command: MockTask,
    },
    /// Mutation-test the code that differs from a base revision.
    Mutants {
        /// The revision to compare the working tree with.
        #[arg(long, default_value = "origin/master")]
        base: String,
    },
}

#[derive(Subcommand)]
enum MockTask {
    Prepare {
        #[arg(long)]
        skip_build: bool,
        #[arg(long)]
        reset: bool,
        #[arg(long, default_value_t = 8765, value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
        #[arg(long, default_value = "valid", value_parser = ["valid", "invalid-signature", "no-update"])]
        scenario: String,
    },
    Serve {
        #[arg(long, default_value_t = 8765, value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
    },
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("Run {}", command.get_program().to_string_lossy()))?;
    if !status.success() {
        // Arguments can contain notarization credentials; never format the command.
        bail!(
            "{} failed with {status}",
            command.get_program().to_string_lossy()
        );
    }
    Ok(())
}

fn output(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("Run {}", command.get_program().to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "{} failed: {}",
            command.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn stable_version(version: &str) -> Result<()> {
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|p| {
            p.is_empty()
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
        })
    {
        bail!("Expected a stable major.minor.patch version, got {version}");
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = root();
    std::env::set_current_dir(&root)?;
    match cli.command {
        Task::Bundle {
            release,
            universal,
            mock_updates,
            mac_app_store,
            dmg,
            prebuilt,
        } => {
            let app = macos::bundle(
                &root,
                macos::BundleOptions {
                    release,
                    universal,
                    mock_updates,
                    mac_app_store,
                    prebuilt,
                },
            )?;
            println!("Built {} (not installed).", app.display());
            if dmg {
                let image = app.with_extension("dmg");
                macos::dmg(&app, &image)?;
                if let Some(identity) = std::env::var("MARKRAFT_SIGN_IDENTITY")
                    .ok()
                    .filter(|identity| identity != "-")
                {
                    macos::sign_image(&image, &identity)?;
                }
                println!("Built {} (not notarized).", image.display());
            }
            Ok(())
        }
        Task::Compile {
            target,
            mac_app_store,
        } => {
            let binary = macos::compile(&root, &target, mac_app_store)?;
            println!("Built {}.", binary.display());
            Ok(())
        }
        Task::Release { tag, prebuilt } => release::release(&root, &tag, prebuilt),
        Task::Mock {
            command:
                MockTask::Prepare {
                    skip_build,
                    reset,
                    port,
                    scenario,
                },
        } => mock::prepare(&root, skip_build, reset, port, &scenario),
        Task::Mock {
            command: MockTask::Serve { port },
        } => mock::serve(&root, port),
        Task::Mutants { base } => mutants::run(&root, &base),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #[test]
    fn only_stable_versions_can_be_published() {
        for version in ["0.1.0", "2.10.100"] {
            assert!(super::stable_version(version).is_ok());
        }
        for version in [
            "1.0",
            "01.2.3",
            "1.2.3-beta",
            "1.2.3+meta",
            "v1.2.3",
            "1..3",
        ] {
            assert!(super::stable_version(version).is_err());
        }
    }
}
