use crate::paths::{ensure_parent, Paths};
use anyhow::{anyhow, Result};
use rand::RngCore;
use std::fs;
use std::sync::OnceLock;

static KEY_CACHE: OnceLock<[u8; KEY_LEN]> = OnceLock::new();

pub const KEY_LEN: usize = 32;
const SERVICE: &str = "com.ripor.RiporAgent";
const ACCOUNT: &str = "queue-key";

pub trait KeyBackend {
    fn get(&self) -> Result<Option<Vec<u8>>>;
    fn set(&self, key: &[u8]) -> Result<()>;
}

pub struct OsKeystore;

impl KeyBackend for OsKeystore {
    fn get(&self) -> Result<Option<Vec<u8>>> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| anyhow!("keystore: {e}"))?;
        match entry.get_password() {
            Ok(hexkey) => Ok(Some(hex::decode(hexkey)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!("keystore get: {e}")),
        }
    }
    fn set(&self, key: &[u8]) -> Result<()> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| anyhow!("keystore: {e}"))?;
        entry
            .set_password(&hex::encode(key))
            .map_err(|e| anyhow!("keystore set: {e}"))
    }
}

pub fn load_or_create_key(paths: &Paths) -> Result<[u8; KEY_LEN]> {
    if let Some(k) = KEY_CACHE.get() {
        return Ok(*k);
    }
    let k = load_or_create_key_with(&OsKeystore, paths)?;
    Ok(*KEY_CACHE.get_or_init(|| k))
}

pub fn load_or_create_key_with(backend: &dyn KeyBackend, paths: &Paths) -> Result<[u8; KEY_LEN]> {
    // 1) clave ya en keystore
    match backend.get() {
        Ok(Some(k)) if k.len() == KEY_LEN => {
            let mut out = [0u8; KEY_LEN];
            out.copy_from_slice(&k);
            // La clave ya vive en el keystore: si quedó un key.bin colgado de
            // una migración previa cuyo borrado falló, lo limpiamos ahora
            // (best-effort; la clave ya está a salvo en el keystore).
            let legacy = paths.key_file();
            if legacy.exists() {
                if let Err(e) = fs::remove_file(&legacy) {
                    tracing::warn!(?e, "no se pudo borrar key.bin residual (clave ya en keystore)");
                }
            }
            return Ok(out);
        }
        Ok(Some(_)) => return Err(anyhow!("clave en keystore con tamaño inválido")),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(?e, "keystore no disponible; fallback a archivo");
            return file_key_fallback(paths, !paths.queue_db().exists());
        }
    }
    // 2) migración desde key.bin legacy
    let legacy = paths.key_file();
    if legacy.exists() {
        let data = fs::read(&legacy)?;
        if data.len() != KEY_LEN {
            return Err(anyhow!("tamaño de clave legacy inválido"));
        }
        let mut k = [0u8; KEY_LEN];
        k.copy_from_slice(&data);
        if let Err(e) = backend.set(&k) {
            tracing::warn!(?e, "keystore set falló; se mantiene key.bin");
            return Ok(k);
        }
        if let Err(e) = fs::remove_file(&legacy) {
            tracing::warn!(?e, "no se pudo borrar key.bin tras migrar; la clave ya está en el keystore");
        }
        tracing::info!("clave migrada de key.bin al keystore del SO");
        return Ok(k);
    }
    // 3) generar nueva — jamás sobre datos cifrados previos
    if paths.queue_db().exists() {
        return Err(anyhow!(
            "keystore sin clave y existen datos cifrados previos (queue.sqlite); no se genera clave nueva para no dejar la cola indescifrable"
        ));
    }
    let mut k = [0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut k);
    if let Err(e) = backend.set(&k) {
        tracing::warn!(?e, "keystore set falló; fallback a archivo 0600");
        return file_key_fallback(paths, !paths.queue_db().exists());
    }
    Ok(k)
}

fn file_key_fallback(paths: &Paths, allow_generate: bool) -> Result<[u8; KEY_LEN]> {
    let key_path = paths.key_file();
    if key_path.exists() {
        let data = fs::read(&key_path)?;
        if data.len() != KEY_LEN {
            return Err(anyhow!("tamaño de clave inválido"));
        }
        enforce_0600(&key_path)?;
        let mut k = [0u8; KEY_LEN];
        k.copy_from_slice(&data);
        return Ok(k);
    }
    if !allow_generate {
        return Err(anyhow!(
            "keystore inaccesible y existen datos cifrados previos (queue.sqlite); no se genera clave nueva para no dejar la cola indescifrable"
        ));
    }
    let mut k = [0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut k);
    ensure_parent(&key_path)?;
    fs::write(&key_path, &k)?;
    enforce_0600(&key_path)?;
    Ok(k)
}

fn enforce_0600(_path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(_path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;
    use std::sync::Mutex;

    struct MemBackend {
        store: Mutex<Option<Vec<u8>>>,
    }
    impl MemBackend {
        fn new() -> Self {
            Self { store: Mutex::new(None) }
        }
    }
    impl KeyBackend for MemBackend {
        fn get(&self) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(self.store.lock().unwrap().clone())
        }
        fn set(&self, key: &[u8]) -> anyhow::Result<()> {
            *self.store.lock().unwrap() = Some(key.to_vec());
            Ok(())
        }
    }

    struct FailBackend;
    impl KeyBackend for FailBackend {
        fn get(&self) -> anyhow::Result<Option<Vec<u8>>> {
            Err(anyhow::anyhow!("sin keystore"))
        }
        fn set(&self, _key: &[u8]) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("sin keystore"))
        }
    }

    fn tmp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { data_dir: dir.path().to_path_buf() };
        (dir, paths)
    }

    #[test]
    fn genera_clave_y_la_guarda_en_keystore_sin_archivo() {
        let (_d, paths) = tmp_paths();
        let backend = MemBackend::new();
        let k1 = load_or_create_key_with(&backend, &paths).unwrap();
        assert!(!paths.key_file().exists(), "no debe crear key.bin");
        let k2 = load_or_create_key_with(&backend, &paths).unwrap();
        assert_eq!(k1, k2, "segunda llamada devuelve la misma clave");
    }

    #[test]
    fn migra_key_bin_legacy_al_keystore_y_lo_borra() {
        let (_d, paths) = tmp_paths();
        let legacy: [u8; 32] = [42u8; 32];
        std::fs::write(paths.key_file(), legacy).unwrap();
        let backend = MemBackend::new();
        let k = load_or_create_key_with(&backend, &paths).unwrap();
        assert_eq!(k, legacy, "conserva la clave legacy");
        assert!(!paths.key_file().exists(), "key.bin debe eliminarse tras migrar");
        assert_eq!(backend.get().unwrap().unwrap(), legacy.to_vec());
    }

    #[test]
    fn fallback_a_archivo_0600_si_keystore_falla() {
        let (_d, paths) = tmp_paths();
        let k1 = load_or_create_key_with(&FailBackend, &paths).unwrap();
        assert!(paths.key_file().exists(), "fallback escribe archivo");
        let k2 = load_or_create_key_with(&FailBackend, &paths).unwrap();
        assert_eq!(k1, k2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.key_file()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "archivo debe ser 0600");
        }
    }

    #[test]
    fn clave_existente_en_keystore_se_reusa_sin_tocar_disco() {
        let (_d, paths) = tmp_paths();
        let backend = MemBackend::new();
        let existente: [u8; 32] = [7u8; 32];
        backend.set(&existente).unwrap();
        let k = load_or_create_key_with(&backend, &paths).unwrap();
        assert_eq!(k, existente);
        assert!(!paths.key_file().exists());
    }

    #[test]
    fn clave_existente_en_keystore_limpia_key_bin_residual() {
        // Simula el caso de una migración previa cuyo fs::remove_file(key.bin)
        // falló (p.ej. permisos transitorios): la clave ya está en el
        // keystore, pero key.bin quedó abandonado en disco. La próxima carga
        // exitosa desde el keystore debe limpiarlo (best-effort).
        let (_d, paths) = tmp_paths();
        let backend = MemBackend::new();
        let existente: [u8; 32] = [11u8; 32];
        backend.set(&existente).unwrap();
        std::fs::write(paths.key_file(), [99u8; 32]).unwrap();
        assert!(paths.key_file().exists());
        let k = load_or_create_key_with(&backend, &paths).unwrap();
        assert_eq!(k, existente, "debe devolver la clave del keystore, no la de key.bin");
        assert!(!paths.key_file().exists(), "key.bin residual debe borrarse");
    }

    struct SetFailBackend;
    impl KeyBackend for SetFailBackend {
        fn get(&self) -> anyhow::Result<Option<Vec<u8>>> { Ok(None) }
        fn set(&self, _key: &[u8]) -> anyhow::Result<()> { Err(anyhow::anyhow!("set falla")) }
    }

    #[test]
    fn migracion_conserva_key_bin_si_set_falla() {
        let (_d, paths) = tmp_paths();
        let legacy: [u8; 32] = [9u8; 32];
        std::fs::write(paths.key_file(), legacy).unwrap();
        let k = load_or_create_key_with(&SetFailBackend, &paths).unwrap();
        assert_eq!(k, legacy, "devuelve la clave legacy");
        assert!(paths.key_file().exists(), "key.bin NO debe borrarse si el set falla");
    }

    #[test]
    fn keystore_caido_con_datos_previos_no_fabrica_clave() {
        let (_d, paths) = tmp_paths();
        std::fs::write(paths.queue_db(), b"datos").unwrap(); // simula cola previa
        let res = load_or_create_key_with(&FailBackend, &paths);
        assert!(res.is_err(), "no debe generar clave nueva con datos cifrados previos y sin key file");
    }

    #[test]
    fn keystore_vacio_con_datos_previos_no_fabrica_clave() {
        let (_d, paths) = tmp_paths();
        std::fs::write(paths.queue_db(), b"datos").unwrap();
        let res = load_or_create_key_with(&MemBackend::new(), &paths);
        assert!(res.is_err(), "keystore vacío + datos previos + sin key file no debe fabricar clave");
    }

    #[test]
    fn keystore_caido_reusa_key_bin_existente_y_fuerza_0600() {
        let (_d, paths) = tmp_paths();
        let legacy: [u8; 32] = [5u8; 32];
        std::fs::write(paths.key_file(), legacy).unwrap();
        let k = load_or_create_key_with(&FailBackend, &paths).unwrap();
        assert_eq!(k, legacy);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.key_file()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "debe forzar 0600 también en archivo preexistente");
        }
    }
}
