pub use crate::keystore::load_or_create_key;
use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm, Nonce};
use anyhow::{anyhow, Result};
use rand::RngCore;

const KEY_LEN: usize = 32; // AES-256-GCM
const NONCE_LEN: usize = 12; // 96-bit nonce
const MAGIC: &[u8] = b"EV1"; // formato cifrado versión 1

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_cifrar_descifrar() {
        let key = [7u8; 32];
        let aad = b"device-1";
        let plain = br#"{"hola":true}"#;
        let blob = encrypt_compress(&key, aad, plain).unwrap();
        let out = decrypt_decompress(&key, aad, &blob).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn descifrar_falla_con_aad_distinto() {
        let key = [7u8; 32];
        let blob = encrypt_compress(&key, b"a", b"x").unwrap();
        assert!(decrypt_decompress(&key, b"b", &blob).is_err());
    }
}
