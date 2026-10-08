fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        if std::env::var_os("CARGO_FEATURE_NATIVE_TRANSLATION").is_some() {
            build_translation();
        }
    }
}

fn build_translation() {
    use std::path::PathBuf;
    use std::process::Command;

    const SOURCE: &str = "src/platform/translation.swift";
    const PRESENTATION: &str = "src/platform/native_presentation.swift";
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed={PRESENTATION}");
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let architecture = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("unsupported macOS translation target: {other:?}"),
    };
    let status = Command::new("xcrun")
        .args([
            "swiftc",
            "-emit-library",
            "-static",
            "-parse-as-library",
            "-module-name",
            "MarkraftTranslation",
            "-target",
        ])
        .arg(format!("{architecture}-apple-macos13.0"))
        .arg(SOURCE)
        .arg(PRESENTATION)
        .arg("-o")
        .arg(output.join("libMarkraftTranslation.a"))
        .status()
        .expect("Xcode Swift compiler is required for macOS text translation");
    assert!(
        status.success(),
        "failed to compile the system translation bridge"
    );
    let swiftc = Command::new("xcrun")
        .args(["--find", "swiftc"])
        .output()
        .expect("locate Xcode Swift compiler");
    assert!(
        swiftc.status.success(),
        "failed to locate the Swift compiler"
    );
    let swiftc = PathBuf::from(
        String::from_utf8(swiftc.stdout)
            .expect("Swift compiler path is UTF-8")
            .trim(),
    );
    let runtime = swiftc
        .parent()
        .and_then(|path| path.parent())
        .expect("Swift compiler belongs to an Xcode toolchain")
        .join("lib/swift/macosx");
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-search=native={}", runtime.display());
    println!("cargo:rustc-link-lib=static=MarkraftTranslation");
    println!("cargo:rustc-link-lib=dylib=swiftCore");
    println!("cargo:rustc-link-lib=framework=SwiftUI");
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,Translation");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
