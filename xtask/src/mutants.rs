//! Mutation testing over what changed: whether the tests would notice if the
//! code just written were wrong, which passing tests alone do not show.

use anyhow::{Context, Result, bail};
use std::{fs, path::Path, process::Command};

/// The crates whose tests run on cargo-mutants' copy of the tree. The app's
/// cannot: its build links the Sparkle framework from `target/`, which the copy
/// leaves out, so its end-to-end tests are not part of this check.
const PACKAGES: &[&str] = &[
    "markraft-core",
    "markraft-commonmark",
    "markraft-gpui",
    "markraft-vim",
];

/// Mutate the code that differs from `base`, working tree included, and run
/// those crates' tests against each mutant.
pub fn run(root: &Path, base: &str) -> Result<()> {
    let installed = Command::new("cargo")
        .args(["mutants", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !installed {
        bail!("Install cargo-mutants first: cargo install cargo-mutants --locked");
    }
    // A patch whatever the user's git prints diffs as: an external diff tool
    // or colour would give cargo-mutants nothing it can read.
    let diff = Command::new("git")
        .args(["diff", "--no-ext-diff", "--no-color", "--no-textconv", base])
        .args(["--", "crates"])
        .current_dir(root)
        .output()
        .context("Run git diff")?;
    if !diff.status.success() {
        bail!(
            "git diff {base} failed: {}",
            String::from_utf8_lossy(&diff.stderr)
        );
    }
    if diff.stdout.is_empty() {
        println!("No code under crates/ differs from {base}.");
        return Ok(());
    }
    let out = root.join("target/mutants");
    fs::create_dir_all(&out)?;
    // Written as git printed it: trimming the last newline would cut the patch.
    let patch = out.join("changes.diff");
    fs::write(&patch, &diff.stdout)?;
    let mut command = Command::new("cargo");
    command
        .args(["mutants", "--in-diff"])
        .arg(&patch)
        .arg("--output")
        .arg(&out)
        .args(["--jobs", "4", "--minimum-test-timeout", "60"]);
    for package in PACKAGES {
        command.args(["--package", package]);
    }
    let status = command.status().context("Run cargo mutants")?;
    match status.code() {
        Some(0) => Ok(()),
        // cargo-mutants' own code for "some mutants were not caught".
        Some(2) => bail!(
            "Some mutants survived: the tests would pass with that code wrong. See {}",
            out.join("mutants.out/missed.txt").display()
        ),
        _ => bail!("cargo mutants failed with {status}"),
    }
}
