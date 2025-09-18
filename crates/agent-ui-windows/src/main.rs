#![cfg(target_os = "windows")]

use std::thread;
use std::time::Duration;
use tray_icon::{TrayIconBuilder, menu::{Menu, MenuItem, MenuEvent}};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::Foundation::{HWND, HINSTANCE, PWSTR};
use winreg::enums::*;
use winreg::RegKey;

fn open_url(url: &str) {
    let wurl: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { let _ = ShellExecuteW(HWND(0), PWSTR("open\0".encode_utf16().collect::<Vec<u16>>().as_ptr() as *mut _), PWSTR(wurl.as_ptr() as *mut _), PWSTR(std::ptr::null_mut()), PWSTR(std::ptr::null_mut()), 1); }
}

fn api_base() -> String {
    std::env::var("PANEL_ADDR").map(|a| format!("http://{}", a)).unwrap_or_else(|_| "http://127.0.0.1:49219".to_string())
}

fn http_get(path: &str) { let base = api_base(); let url = format!("{}{}", base, path); thread::spawn(move || { let _ = reqwest::blocking::get(&url); }); }

fn autorun_enabled() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run") {
        return key.get_value::<String,_>("RiporAgent").is_ok();
    }
    false
}

fn set_autorun(enable: bool) {
    let exe = std::env::current_exe().unwrap_or_default();
    let mut exe_dir = exe.clone();
    // Prefer ejecutar el daemon si está junto al UI (ej. bundle)
    let agent = exe_dir.with_file_name("agent-daemon.exe");
    let target = if agent.exists() { agent } else { exe };
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run").unwrap();
    if enable { let _ = key.set_value("RiporAgent", &target.display().to_string()); }
    else { let _ = key.delete_value("RiporAgent"); }
}

fn main() {
    // Build menu
    let mut menu = Menu::new();
    let open_panel = MenuItem::new("Ver panel", true, None);
    let pause15 = MenuItem::new("Pausar 15 min", true, None);
    let pause60 = MenuItem::new("Pausar 60 min", true, None);
    let resume = MenuItem::new("Reanudar", true, None);
    let sep = MenuItem::new("-", false, None);
    let auto_toggle = MenuItem::new("Iniciar al abrir sesión", true, None);
    let quit = MenuItem::new("Salir", true, None);
    menu.append(&open_panel).unwrap();
    menu.append(&sep).unwrap();
    menu.append(&pause15).unwrap();
    menu.append(&pause60).unwrap();
    menu.append(&resume).unwrap();
    menu.append(&sep).unwrap();
    menu.append(&auto_toggle).unwrap();
    menu.append(&sep).unwrap();
    menu.append(&quit).unwrap();

    // Try to load bundled .ico (fallback: transparent 32x32)
    let icon = load_tray_icon();
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("RiporAgent")
        .with_icon(icon)
        .build()
        .unwrap();

    // Init autorun state (we can't set checkmark, but we can toggle label)
    let mut auto_on = autorun_enabled();
    let _ = auto_toggle.set_text(if auto_on { "Desactivar inicio automático" } else { "Activar inicio automático" });

    let rx = MenuEvent::receiver();
    loop {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(250)) {
            let id = ev.id;
            if id == open_panel.id() { open_url(&(api_base()+"/ui")); }
            else if id == pause15.id() { http_get("/pause?minutes=15"); }
            else if id == pause60.id() { http_get("/pause?minutes=60"); }
            else if id == resume.id() { http_get("/pause/clear"); }
            else if id == auto_toggle.id() {
                set_autorun(!auto_on); auto_on = autorun_enabled();
                let _ = auto_toggle.set_text(if auto_on { "Desactivar inicio automático" } else { "Activar inicio automático" });
            }
            else if id == quit.id() { break; }
        }
    }
}

fn load_tray_icon() -> Option<tray_icon::Icon> {
    // Prefer embedded bytes at compile time
    let data: Option<&'static [u8]> = option_env!("RIPOR_NO_EMBED_ICON").map(|_| None).unwrap_or_else(|| {
        // Embed relative to crate dir; if path invalid, compiler will error unless disabled
        // To allow builds without asset, feature gate via env or use include_bytes! in a try-block
        // We'll attempt include_bytes and rely on Cargo paths.
        let bytes: Option<&'static [u8]> = {
            #[allow(unused_mut)]
            let mut b: Option<&'static [u8]> = None;
            // Use conditional compilation to avoid hard failure when asset path missing during dev
            // If this path is wrong, set RIPOR_NO_EMBED_ICON=1 to skip embedding.
            b = Some(include_bytes!("../../assets/icons/windows/icon.ico"));
            b
        };
        bytes
    });
    if let Some(bytes) = data {
        if let Ok(icon) = decode_ico(bytes) { return Some(icon); }
    }
    // Fallback transparent
    let rgba = vec![0u8; 32*32*4];
    tray_icon::Icon::from_rgba(rgba, 32, 32).ok()
}

fn decode_ico(bytes: &[u8]) -> Result<tray_icon::Icon, ()> {
    let mut cursor = std::io::Cursor::new(bytes);
    let ico = ico::IconDir::read(&mut cursor).map_err(|_| ())?;
    // choose biggest image
    let entry = ico.entries().iter().max_by_key(|e| e.width()).ok_or(())?;
    let image = entry.decode().map_err(|_| ())?;
    let rgba = match image {
        ico::IconImage::Bmp(bmp) => bmp.rgba_data().to_vec(),
        ico::IconImage::Png(png) => png,
    };
    let w = entry.width() as u32; let h = entry.height() as u32;
    tray_icon::Icon::from_rgba(rgba, w, h).map_err(|_| ())
}
