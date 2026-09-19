use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

fn public_key(encoded: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = STANDARD
        .decode(encoded)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("An Ed25519 public key must contain 32 bytes"))?;
    VerifyingKey::from_bytes(&bytes).context("Invalid Ed25519 public key")
}

pub fn validate_public_key(encoded: &str) -> Result<()> {
    public_key(encoded).map(|_| ())
}

pub fn verify_archive(archive: &Path, key: &str, signature: &str) -> Result<()> {
    let signature = Signature::from_slice(&STANDARD.decode(signature)?)?;
    public_key(key)?
        .verify_strict(&fs::read(archive)?, &signature)
        .context("Update signature does not match the application's public key")
}

pub fn generate_key(path: &Path) -> Result<String> {
    let mut seed = [0; 32];
    File::open("/dev/urandom")?.read_exact(&mut seed)?;
    let key = SigningKey::from_bytes(&seed);
    // Sparkle 2.9 accepts a base64-encoded 32-byte seed exported by generate_keys.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(STANDARD.encode(seed).as_bytes())?;
    ensure!(
        file.metadata()?.len() > 0,
        "Failed to write mock signing key"
    );
    Ok(STANDARD.encode(key.verifying_key().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    #[test]
    fn verifies_archive_with_embedded_public_key_and_rejects_tampering() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("update.zip");
        fs::write(&archive, b"archive")?;
        let key = SigningKey::from_bytes(&[7; 32]);
        let signature = STANDARD.encode(key.sign(b"archive").to_bytes());
        let public = STANDARD.encode(key.verifying_key().as_bytes());
        verify_archive(&archive, &public, &signature)?;
        let wrong_key = SigningKey::from_bytes(&[8; 32]);
        assert!(
            verify_archive(
                &archive,
                &STANDARD.encode(wrong_key.verifying_key().as_bytes()),
                &signature
            )
            .is_err()
        );
        fs::write(&archive, b"tampered")?;
        assert!(verify_archive(&archive, &public, &signature).is_err());
        Ok(())
    }
}
