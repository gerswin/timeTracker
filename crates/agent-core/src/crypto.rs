use crate::paths::{ensure_parent, Paths};
use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm, Nonce};
use anyhow::{anyhow, Result};
use rand::RngCore;
use std::fs;

const KEY_LEN: usize = 32; // AES-256-GCM
const NONCE_LEN: usize = 12; // 96-bit nonce
const MAGIC: &[u8] = b"EV1"; // formato cifrado versión 1

pub fn load_or_create_key(paths: &Paths) -> Result<[u8; KEY_LEN]> {
    let key_path = paths.key_file();
    if key_path.exists() {
        let data = fs::read(&key_path)?;
        if data.len() != KEY_LEN {
            return Err(anyhow!("tamaño de clave inválido"));
        }
        let mut k = [0u8; KEY_LEN];
        k.copy_from_slice(&data);
        return Ok(k);
    }
    let mut k = [0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut k);
    ensure_parent(&key_path)?;
    fs::write(&key_path, &k)?;
    Ok(k)
}

pub fn encrypt_compress(key: &[u8; KEY_LEN], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("clave AES inválida"))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let compressed = zstd::encode_all(plaintext, 3)?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + compressed.len() + 16);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, aes_gcm::aead::Payload { msg: &compressed, aad })
        .map_err(|_| anyhow!("falló cifrado"))?;
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt_decompress(key: &[u8; KEY_LEN], aad: &[u8], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < MAGIC.len() + NONCE_LEN + 16 || &blob[..MAGIC.len()] != MAGIC {
        return Err(anyhow!("formato inválido"));
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("clave AES inválida"))?;
    let nonce = Nonce::from_slice(&blob[MAGIC.len()..MAGIC.len() + NONCE_LEN]);
    let ct = &blob[MAGIC.len() + NONCE_LEN..];
    let compressed = cipher
        .decrypt(nonce, aes_gcm::aead::Payload { msg: ct, aad })
        .map_err(|_| anyhow!("falló descifrado"))?;
    let decompressed = zstd::decode_all(&compressed[..])?;
    Ok(decompressed)
}
