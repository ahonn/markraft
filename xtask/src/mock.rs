use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const APP_NAME: &str = "Markraft Update Test.app";
const BUNDLE_ID: &str = "dev.markraft.update-test";
const SCENARIOS: &[&str] = &["valid", "invalid-signature", "no-update"];

pub fn prepare(
    root: &Path,
    skip_build: bool,
    reset: bool,
    port: u16,
    scenario: &str,
) -> Result<()> {
    ensure!(port != 0, "port must be between 1 and 65535");
    ensure!(
        SCENARIOS.contains(&scenario),
        "unknown scenario: {scenario}"
    );
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is missing")?);
    let work = root.join("target/mock-updates");
    let install = home.join("Applications").join(APP_NAME);
    let settings = home.join("Library/Application Support/Markraft Update Test/settings.json");
    check_collisions(&work, &install, &settings, reset)?;

    let source = if skip_build {
        root.join("target/debug/bundle/osx/Markraft.app")
    } else {
        crate::macos::bundle(
            root,
            crate::macos::BundleOptions {
                release: false,
                universal: false,
                mock_updates: true,
            },
        )?
    };
    let source_info = plist::Value::from_file(source.join("Contents/Info.plist"))?;
    ensure!(
        source_info
            .as_dictionary()
            .and_then(|d| d.get("MarkraftMockUpdates"))
            == Some(&plist::Value::Boolean(true)),
        "source app lacks MarkraftMockUpdates=true; build with --mock-updates"
    );

    fs::create_dir_all(root.join("target"))?;
    let staging = tempfile::Builder::new()
        .prefix(".mock-updates-")
        .tempdir_in(root.join("target"))?;
    let prepared = staging.path().join("prepared");
    fs::create_dir(&prepared)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&prepared, fs::Permissions::from_mode(0o700))?;
    }
    let key = prepared.join("private-key.txt");
    let public_key = crate::crypto::generate_key(&key)?;
    let public = prepared.join("public");
    fs::create_dir(&public)?;
    let base = format!("http://127.0.0.1:{port}");
    for (version, feed_name) in [("0.1.0", "no-update"), ("0.1.1", "valid")] {
        let version_dir = prepared.join(version);
        fs::create_dir(&version_dir)?;
        let app = version_dir.join(APP_NAME);
        crate::run(Command::new("ditto").arg(&source).arg(&app))?;
        configure_fixture(&app, version, &public_key, port)?;
        crate::macos::sign(&app, "-")?;
        let archives = version_dir.join("archives");
        fs::create_dir(&archives)?;
        let name = format!("Markraft-Update-Test-{version}.zip");
        let archive = archives.join(&name);
        crate::macos::zip(&app, &archive)?;
        let feed = crate::release::generate_appcast(
            root,
            &archives,
            &key,
            &format!("{base}/"),
            &format!("{base}/release-notes.html"),
        )?;
        let xml = fs::read_to_string(feed)?;
        let signature = signature_value(&xml)?.1;
        crate::crypto::verify_archive(&archive, &public_key, signature)?;
        fs::write(public.join(format!("{feed_name}.xml")), &xml)?;
        if feed_name == "valid" {
            fs::write(
                public.join("invalid-signature.xml"),
                invalid_signature_feed(&xml)?,
            )?;
        }
        fs::rename(archive, public.join(name))?;
    }
    fs::write(
        public.join("release-notes.html"),
        "<!doctype html><html><body><h1>Local Sparkle Update Test</h1><p>Isolated test release 0.1.1. Notes should survive installation and relaunch.</p></body></html>\n",
    )?;
    fs::write(prepared.join("scenario.txt"), format!("{scenario}\n"))?;
    fs::write(
        prepared.join("config.json"),
        serde_json::to_vec_pretty(&json!({"port": port, "public_key": public_key}))?,
    )?;
    if work.join("notes").exists() {
        crate::run(
            Command::new("ditto")
                .arg(work.join("notes"))
                .arg(prepared.join("notes")),
        )?;
    } else {
        fs::create_dir(prepared.join("notes"))?;
    }

    // Complete expensive build/signing work before replacing any existing fixtures.
    let applications = install.parent().context("missing Applications directory")?;
    fs::create_dir_all(applications)?;
    let app_staging = tempfile::Builder::new()
        .prefix(".markraft-update-")
        .tempdir_in(applications)?;
    let staged_app = app_staging.path().join(APP_NAME);
    crate::run(
        Command::new("ditto")
            .arg(prepared.join("0.1.0").join(APP_NAME))
            .arg(&staged_app),
    )?;
    let settings_dir = settings.parent().context("missing settings directory")?;
    fs::create_dir_all(settings_dir)?;
    let mut staged_settings = tempfile::NamedTempFile::new_in(settings_dir)?;
    serde_json::to_writer_pretty(
        &mut staged_settings,
        &json!({"notes_folder": work.join("notes")}),
    )?;
    staged_settings.flush()?;
    let old_work = staging.path().join("previous");
    let old_app = app_staging.path().join("previous.app");
    let had_work = work.exists();
    let had_app = install.exists();
    let mut work_replaced = false;
    let mut app_replaced = false;
    let result = (|| -> Result<()> {
        if had_work {
            fs::rename(&work, &old_work)?;
        }
        fs::rename(&prepared, &work)?;
        work_replaced = true;
        if had_app {
            fs::rename(&install, &old_app)?;
        }
        fs::rename(&staged_app, &install)?;
        app_replaced = true;
        staged_settings
            .persist(&settings)
            .map_err(|error| error.error)?;
        Ok(())
    })();
    if let Err(error) = result {
        // Keep backups on disk if rollback itself fails, rather than letting TempDir delete them.
        let rollback = (|| -> Result<()> {
            if app_replaced {
                fs::remove_dir_all(&install)?;
            }
            if old_app.exists() {
                fs::rename(&old_app, &install)?;
            }
            if work_replaced {
                fs::remove_dir_all(&work)?;
            }
            if old_work.exists() {
                fs::rename(&old_work, &work)?;
            }
            Ok(())
        })();
        if let Err(rollback_error) = rollback {
            let backup_work = staging.keep();
            let backup_app = app_staging.keep();
            bail!(
                "preparation failed: {error:#}; rollback failed: {rollback_error:#}; backups retained at {} and {}",
                backup_work.display(),
                backup_app.display()
            );
        }
        return Err(
            error.context("preparation failed; previous installation and fixtures restored")
        );
    }
    println!("Installed baseline: {}", install.display());
    println!("Feed: {base}/appcast.xml");
    println!("Next: cargo xtask mock serve --port {port}");
    println!(
        "Change live scenario: write {} to {}",
        SCENARIOS.join(", "),
        work.join("scenario.txt").display()
    );
    Ok(())
}

fn check_collisions(work: &Path, install: &Path, settings: &Path, reset: bool) -> Result<()> {
    for path in [work, install, settings] {
        ensure!(
            !path.is_symlink(),
            "refusing to replace symlink {}",
            path.display()
        );
        ensure!(
            reset || !path.exists(),
            "{} already exists; use --reset to replace isolated test fixtures",
            path.display()
        );
    }
    if install.exists() {
        let info = plist::Value::from_file(install.join("Contents/Info.plist"))?;
        ensure!(
            info.as_dictionary()
                .and_then(|d| d.get("CFBundleIdentifier"))
                .and_then(plist::Value::as_string)
                == Some(BUNDLE_ID),
            "refusing to replace an app without the isolated test bundle identifier"
        );
    }
    Ok(())
}

fn configure_fixture(app: &Path, version: &str, key: &str, port: u16) -> Result<()> {
    let path = app.join("Contents/Info.plist");
    let mut info = plist::Value::from_file(&path)?;
    let dict = info
        .as_dictionary_mut()
        .context("Info.plist must be a dictionary")?;
    for (name, value) in [
        ("CFBundleIdentifier", BUNDLE_ID.to_owned()),
        ("CFBundleName", "Markraft Update Test".to_owned()),
        ("CFBundleDisplayName", "Markraft Update Test".to_owned()),
        ("CFBundleVersion", version.to_owned()),
        ("CFBundleShortVersionString", version.to_owned()),
        ("SUFeedURL", format!("http://127.0.0.1:{port}/appcast.xml")),
        ("SUPublicEDKey", key.to_owned()),
    ] {
        dict.insert(name.into(), plist::Value::String(value));
    }
    dict.insert(
        "SUEnableAutomaticChecks".into(),
        plist::Value::Boolean(false),
    );
    dict.insert("SUAutomaticallyUpdate".into(), plist::Value::Boolean(false));
    dict.insert(
        "SUScheduledCheckInterval".into(),
        plist::Value::Integer(86400.into()),
    );
    let mut ats = plist::Dictionary::new();
    ats.insert(
        "NSAllowsLocalNetworking".into(),
        plist::Value::Boolean(true),
    );
    dict.insert(
        "NSAppTransportSecurity".into(),
        plist::Value::Dictionary(ats),
    );
    info.to_file_xml(path)?;
    Ok(())
}

fn signature_value(xml: &str) -> Result<(std::ops::Range<usize>, &str)> {
    // The official generator emits exactly one enclosure because max versions is one.
    let marker = "sparkle:edSignature=\"";
    ensure!(
        xml.matches(marker).count() == 1,
        "expected one generated enclosure signature"
    );
    let start = xml.find(marker).context("missing enclosure signature")? + marker.len();
    let end = start
        + xml[start..]
            .find('"')
            .context("unterminated enclosure signature")?;
    Ok((start..end, &xml[start..end]))
}

fn invalid_signature_feed(xml: &str) -> Result<String> {
    let (range, signature) = signature_value(xml)?;
    let mut bytes = STANDARD.decode(signature)?;
    ensure!(bytes.len() == 64, "expected an Ed25519 signature");
    bytes[0] ^= 1;
    let mut invalid = xml.to_owned();
    invalid.replace_range(range, &STANDARD.encode(bytes));
    Ok(invalid)
}

pub fn serve(root: &Path, port: u16) -> Result<()> {
    ensure!(port != 0, "port must be between 1 and 65535");
    let work = root.join("target/mock-updates");
    let config: serde_json::Value = serde_json::from_slice(&fs::read(work.join("config.json"))?)?;
    ensure!(
        config["port"].as_u64() == Some(u64::from(port)),
        "fixtures use port {}; prepare again to change it",
        config["port"]
    );
    let server =
        tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Serving local appcast on http://127.0.0.1:{port}/appcast.xml");
    for request in server.incoming_requests() {
        handle_request(&work, request)?;
    }
    Ok(())
}

fn route(work: &Path, method: &str, url: &str) -> (u16, &'static str, Vec<u8>) {
    if method != "GET" && method != "HEAD" {
        return (405, "text/plain", b"Method not allowed\n".to_vec());
    }
    let path = url.split('?').next().unwrap_or(url);
    let (file, content_type) = match path {
        "/appcast.xml" => {
            let scenario = match fs::read_to_string(work.join("scenario.txt")) {
                Ok(value) if SCENARIOS.contains(&value.trim()) => value.trim().to_owned(),
                _ => return (500, "text/plain", b"Invalid local test scenario\n".to_vec()),
            };
            (format!("{scenario}.xml"), "application/xml")
        }
        "/release-notes.html" => ("release-notes.html".into(), "text/html; charset=utf-8"),
        "/Markraft-Update-Test-0.1.0.zip" | "/Markraft-Update-Test-0.1.1.zip" => {
            (path[1..].into(), "application/zip")
        }
        _ => return (404, "text/plain", b"Not found\n".to_vec()),
    };
    let file = work.join("public").join(file);
    if file.is_symlink() {
        return (404, "text/plain", b"Not found\n".to_vec());
    }
    match fs::read(file) {
        Ok(bytes) => (200, content_type, bytes),
        Err(_) => (404, "text/plain", b"Not found\n".to_vec()),
    }
}

fn handle_request(work: &Path, request: tiny_http::Request) -> Result<()> {
    let (status, content_type, bytes) = route(work, request.method().as_str(), request.url());
    let mut log = OpenOptions::new()
        .append(true)
        .create(true)
        .open(work.join("access.log"))?;
    writeln!(
        log,
        "{} {:?} {} {} {}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        request.remote_addr(),
        request.method(),
        request.url(),
        status
    )?;
    let response = tiny_http::Response::from_data(bytes)
        .with_status_code(status)
        .with_header(
            tiny_http::Header::from_bytes("Content-Type", content_type)
                .expect("static valid header"),
        )
        .with_header(
            tiny_http::Header::from_bytes("Cache-Control", "no-store")
                .expect("static valid header"),
        );
    // A client disconnect must not stop the test server.
    if let Err(error) = request.respond(response) {
        eprintln!("mock response failed: {error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn only_signature_changes_for_invalid_feed() {
        let signature = STANDARD.encode([0u8; 64]);
        let xml = format!(
            "<enclosure url=\"http://127.0.0.1:8765/app.zip\" sparkle:edSignature=\"{signature}\" length=\"123\" />"
        );
        let invalid = invalid_signature_feed(&xml).unwrap();
        let (_, changed) = signature_value(&invalid).unwrap();
        let mut expected = [0u8; 64];
        expected[0] = 1;
        assert_eq!(STANDARD.decode(changed).unwrap(), expected);
        assert_eq!(invalid.replace(changed, &signature), xml);
        assert!(invalid_signature_feed("<rss />").is_err());
        assert!(invalid_signature_feed(&format!("{xml}{xml}")).is_err());
    }

    #[test]
    fn collisions_require_reset_and_isolated_identity() {
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().join("work");
        let app = temp.path().join("app");
        let settings = temp.path().join("settings");
        fs::create_dir(&work).unwrap();
        assert!(check_collisions(&work, &app, &settings, false).is_err());
        assert!(check_collisions(&work, &app, &settings, true).is_ok());
        fs::create_dir_all(app.join("Contents")).unwrap();
        let mut info = plist::Dictionary::new();
        info.insert("CFBundleIdentifier".into(), "dev.markraft.notes".into());
        plist::Value::Dictionary(info)
            .to_file_xml(app.join("Contents/Info.plist"))
            .unwrap();
        assert!(check_collisions(&work, &app, &settings, true).is_err());
    }

    #[test]
    fn live_server_switches_scenarios_and_hides_private_files() {
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path();
        fs::create_dir(work.join("public")).unwrap();
        fs::write(work.join("private-key.txt"), "secret").unwrap();
        for scenario in SCENARIOS {
            fs::write(
                work.join("public").join(format!("{scenario}.xml")),
                scenario,
            )
            .unwrap();
        }
        let server = tiny_http::Server::http(("127.0.0.1", 0)).unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let server_work = work.to_owned();
        let worker = std::thread::spawn(move || {
            for _ in 0..7 {
                handle_request(
                    &server_work,
                    server
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap()
                        .expect("request within deadline"),
                )
                .unwrap();
            }
        });
        let get = |path: &str| {
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        for scenario in SCENARIOS {
            fs::write(work.join("scenario.txt"), scenario).unwrap();
            let response = get("/appcast.xml?cache=ignored");
            assert!(response.starts_with("HTTP/1.1 200"));
            assert!(response.to_lowercase().contains("cache-control: no-store"));
            assert!(response.ends_with(scenario));
        }
        for path in [
            "/../private-key.txt",
            "/%2e%2e/private-key.txt",
            "/valid.xml",
        ] {
            let response = get(path);
            assert!(response.starts_with("HTTP/1.1 404"));
            assert!(!response.contains("secret"));
        }
        fs::write(work.join("scenario.txt"), "unknown").unwrap();
        assert!(get("/appcast.xml").starts_with("HTTP/1.1 500"));
        worker.join().unwrap();
        assert!(
            fs::read_to_string(work.join("access.log"))
                .unwrap()
                .contains("GET")
        );
    }
}
