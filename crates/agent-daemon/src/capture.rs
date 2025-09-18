use agent_core::queue::Queue;
use agent_core::state::AgentState;
use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::time::{sleep, Duration};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Serialize)]
struct CaptureEvent {
    ts_ms: u64,
    app_name: String,
    window_title: String,
    input_idle_ms: u64,
}

pub async fn run_capture_loop(
    state: Arc<AgentState>,
    paths: &agent_core::paths::Paths,
    last_event_ts: Arc<AtomicU64>,
    last_idle_ms: Arc<AtomicU64>,
    paused_until_ms: Arc<AtomicU64>,
) {
    info!("iniciando loop de captura (Fase 1)");
    println!("[debug] capture loop started");
    let mut prev_app = String::new();
    let mut prev_title = String::new();
    loop {
        debug!("capture tick");
        // Respetar pausa
        let now = now_ms();
        if paused_until_ms.load(Ordering::Relaxed) > now {
            sleep(Duration::from_millis(500)).await;
            continue;
        }
        match sample_once() {
            Ok((app, title, idle_ms)) => {
                last_idle_ms.store(idle_ms, Ordering::Relaxed);
                debug!(app = ?app, title = ?title, idle_ms, "sample actual");
                // Emitir solo en cambio o cada 30s
                let changed = app != prev_app || title != prev_title;
                let force_emit = should_force_emit(last_event_ts.load(Ordering::Relaxed));
                if changed || force_emit {
                    let evt = CaptureEvent { ts_ms: now_ms(), app_name: app.clone(), window_title: title.clone(), input_idle_ms: idle_ms };
                    debug!("abriendo queue para enqueue");
                    if let Ok(q) = Queue::open(paths, &state) {
                        if let Ok(_) = q.enqueue_json(&serde_json::to_vec(&evt).unwrap()) {
                            last_event_ts.store(evt.ts_ms, Ordering::Relaxed);
                            info!(app = ?evt.app_name, title = ?evt.window_title, "captura encolada");
                        } else {
                            warn!("falló enqueue captura");
                        }
                    } else {
                        warn!("falló abrir cola");
                    }
                    prev_app = app;
                    prev_title = title;
                }
            }
            Err(e) => {
                debug!(?e, "sample_once error");
            }
        }
        sleep(Duration::from_millis(1000)).await;
    }
}

fn should_force_emit(last_ts: u64) -> bool {
    if last_ts == 0 { return true; }
    let now = now_ms();
    now.saturating_sub(last_ts) > 30_000
}

#[cfg(target_os = "macos")]
fn sample_once() -> Result<(String, String, u64)> {
    // 0) Preferir AX sistema: app enfocada (más fiable entre Spaces)
    if let Some((ax_pid, ax_name)) = ax_focused_app() {
        let title = cg_front_window_title(ax_pid as i64)
            .or_else(|| ax_window_title(ax_pid))
            .unwrap_or_default();
        if title.is_empty() { perms_diag_once(); }
        let idle_ms = (cg_seconds_since_last_input() * 1000.0).round() as u64;
        return Ok((ax_name, title, idle_ms));
    }
    // 1) CoreGraphics: ventana top (layer 0) → owner y título
    if let Some((owner_name, owner_pid, maybe_title)) = cg_front_window_owner_and_title() {
        let title = maybe_title.or_else(|| ax_window_title(owner_pid as i32)).unwrap_or_default();
        if title.is_empty() { perms_diag_once(); }
        let idle_ms = (cg_seconds_since_last_input() * 1000.0).round() as u64;
        return Ok((owner_name, title, idle_ms));
    }
    // 2) Fallback: NSWorkspace + AX (si CG no devolvió nada)
    use objc::{class, msg_send, sel, sel_impl};
    use objc::runtime::Object;
    unsafe {
        let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() { return Ok((String::new(), String::new(), 0)); }
        let app: *mut Object = msg_send![ws, frontmostApplication];
        if app.is_null() { return Ok((String::new(), String::new(), 0)); }
        let name: *mut Object = msg_send![app, localizedName];
        let app_name = nsstring_to_string(name);
        let pid: i32 = msg_send![app, processIdentifier];
        let title = ax_window_title(pid).unwrap_or_default();
        if title.is_empty() { perms_diag_once(); }
        let idle_ms = (cg_seconds_since_last_input() * 1000.0).round() as u64;
        Ok((app_name, title, idle_ms))
    }
}

#[cfg(target_os = "windows")]
fn sample_once() -> Result<(String, String, u64)> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId};
    use windows::Win32::Foundation::HWND;
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        let mut pid: u32 = 0;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let app_name = proc_name_from_pid(pid).unwrap_or_else(|| "Unknown".to_string());
        let mut buf: [u16; 512] = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        let title = if len > 0 { String::from_utf16_lossy(&buf[..len as usize]) } else { String::new() };
        let idle_ms = windows_idle_ms();
        Ok((app_name, title, idle_ms))
    }
}

#[cfg(target_os = "linux")]
fn sample_once() -> Result<(String, String, u64)> {
    Ok((String::new(), String::new(), 0))
}

#[derive(Debug, Clone, Serialize)]
pub struct SampleDebugDto {
    pub app_name: String,
    pub window_title: String,
    pub input_idle_ms: u64,
    pub title_source: String,
    // Triangulación de foco
    pub ax_pid: Option<i32>,
    pub ax_name: Option<String>,
    pub ns_pid: Option<i32>,
    pub ns_name: Option<String>,
    pub cg_pid: Option<i64>,
    pub cg_owner: Option<String>,
    pub cg_title: Option<String>,
    pub ax_title: Option<String>,
    #[cfg(target_os = "macos")]
    pub perms: super::macos_perms::PermsStatus,
}

#[cfg(target_os = "macos")]
pub fn sample_debug() -> Result<SampleDebugDto> {
    // Triangulación: AX (preferente), luego NS, luego CG
    let ax = ax_focused_app();
    let ns = unsafe {
        use objc::{class, msg_send, sel, sel_impl};
        use objc::runtime::Object;
        let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() { None } else {
            let app: *mut Object = msg_send![ws, frontmostApplication];
            if app.is_null() { None } else {
                let name: *mut Object = msg_send![app, localizedName];
                let app_name = nsstring_to_string(name);
                let pid: i32 = msg_send![app, processIdentifier];
                Some((pid, app_name))
            }
        }
    };
    let cg = cg_front_window_owner_and_title();

    // Efectivo: elegir PID/nombre priorizando AX → NS → CG
    let (eff_pid, eff_name) = if let Some((p,n)) = &ax {
        (*p, n.clone())
    } else if let Some((p,n)) = &ns {
        (*p, n.clone())
    } else if let Some((owner, p, _t)) = &cg {
        (*p as i32, owner.clone())
    } else {
        (0, String::new())
    };

    // Título: intentar CG por PID, luego AX por PID
    let (title, source) = if let Some(t) = cg_front_window_title(eff_pid as i64) {
        (t, "cg")
    } else if let Some(t2) = ax_window_title(eff_pid) {
        (t2, "ax")
    } else {
        (String::new(), "none")
    };
    let idle_ms = (cg_seconds_since_last_input() * 1000.0).round() as u64;
    Ok(SampleDebugDto {
        app_name: eff_name,
        window_title: title,
        input_idle_ms: idle_ms,
        title_source: source.into(),
        ax_pid: ax.as_ref().map(|(p,_)| *p),
        ax_name: ax.as_ref().map(|(_,n)| n.clone()),
        ns_pid: ns.as_ref().map(|(p,_)| *p),
        ns_name: ns.as_ref().map(|(_,n)| n.clone()),
        cg_pid: cg.as_ref().map(|(_,p,_)| *p),
        cg_owner: cg.as_ref().map(|(o,_,_)| o.clone()),
        cg_title: cg_front_window_title(eff_pid as i64),
        ax_title: ax_window_title(eff_pid),
        perms: super::macos_perms::check_permissions(),
    })
}

#[cfg(target_os = "windows")]
pub fn sample_debug() -> Result<SampleDebugDto> {
    let (app, title, idle) = sample_once()?;
    Ok(SampleDebugDto {
        app_name: app,
        window_title: title,
        input_idle_ms: idle,
        title_source: "win".into(),
        ax_pid: None,
        ax_name: None,
        ns_pid: None,
        ns_name: None,
        cg_pid: None,
        cg_owner: None,
        cg_title: None,
        ax_title: None,
    })
}

#[cfg(target_os = "linux")]
pub fn sample_debug() -> Result<SampleDebugDto> {
    Ok(SampleDebugDto {
        app_name: String::new(),
        window_title: String::new(),
        input_idle_ms: 0,
        title_source: "unsupported".into(),
        ax_pid: None,
        ax_name: None,
        ns_pid: None,
        ns_name: None,
        cg_pid: None,
        cg_owner: None,
        cg_title: None,
        ax_title: None,
    })
}

#[cfg(target_os = "windows")]
fn windows_idle_ms() -> u64 {
    use windows::Win32::UI::WindowsAndMessaging::GetLastInputInfo;
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::WindowsAndMessaging::LASTINPUTINFO;
    unsafe {
        let mut lii = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if GetLastInputInfo(&mut lii).as_bool() {
            let now = GetTickCount();
            return now.wrapping_sub(lii.dwTime) as u64;
        }
    }
    0
}

#[cfg(target_os = "windows")]
fn proc_name_from_pid(pid: u32) -> Option<String> {
    let mut sys = sysinfo::System::new();
    sys.refresh_process(sysinfo::Pid::from_u32(pid));
    sys.process(sysinfo::Pid::from_u32(pid)).map(|p| p.name().to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowInfoDto {
    pub owner_name: String,
    pub owner_pid: i64,
    pub layer: i64,
    pub window_title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrontmostDebugDto {
    pub ax_pid: Option<i32>,
    pub ax_name: Option<String>,
    pub ns_pid: Option<i32>,
    pub ns_name: Option<String>,
    pub cg_pid: Option<i64>,
    pub cg_owner: Option<String>,
    pub cg_title: Option<String>,
}

#[cfg(target_os = "macos")]
pub fn frontmost_debug() -> FrontmostDebugDto {
    // AX
    let ax = ax_focused_app();
    // NS
    let ns = unsafe {
        use objc::{class, msg_send, sel, sel_impl};
        use objc::runtime::Object;
        let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() { None } else {
            let app: *mut Object = msg_send![ws, frontmostApplication];
            if app.is_null() { None } else {
                let name: *mut Object = msg_send![app, localizedName];
                let app_name = nsstring_to_string(name);
                let pid: i32 = msg_send![app, processIdentifier];
                Some((pid, app_name))
            }
        }
    };
    // CG
    let cg = cg_front_window_owner_and_title();
    FrontmostDebugDto {
        ax_pid: ax.as_ref().map(|(p,_)| *p),
        ax_name: ax.as_ref().map(|(_,n)| n.clone()),
        ns_pid: ns.as_ref().map(|(p,_)| *p),
        ns_name: ns.as_ref().map(|(_,n)| n.clone()),
        cg_pid: cg.as_ref().map(|(_,p,_)| *p),
        cg_owner: cg.as_ref().map(|(o,_,_)| o.clone()),
        cg_title: cg.as_ref().and_then(|(_,_,t)| t.clone()),
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn frontmost_debug() -> FrontmostDebugDto { FrontmostDebugDto { ax_pid: None, ax_name: None, ns_pid: None, ns_name: None, cg_pid: None, cg_owner: None, cg_title: None } }

#[cfg(target_os = "macos")]
pub fn list_windows_debug(limit: usize) -> Vec<WindowInfoDto> {
    use core_foundation::string::CFString;
    use core_foundation::base::TCFType;
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
    use core_foundation_sys::number::{kCFNumberSInt64Type, CFNumberGetValue, CFNumberRef};
    use core_foundation_sys::string::CFStringRef;

    const K_ONSCREEN_ONLY: u32 = 1; // kCGWindowListOptionOnScreenOnly
    const K_EXCLUDE_DESKTOP: u32 = 16; // kCGWindowListExcludeDesktopElements
    extern "C" { fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> CFArrayRef; }

    let mut out = Vec::new();
    unsafe {
        let arr = CGWindowListCopyWindowInfo(K_ONSCREEN_ONLY | K_EXCLUDE_DESKTOP, 0);
        if arr.is_null() { return out; }
        let count = CFArrayGetCount(arr);
        let key_pid = CFString::from_static_string("kCGWindowOwnerPID");
        let key_layer = CFString::from_static_string("kCGWindowLayer");
        let key_name = CFString::from_static_string("kCGWindowName");
        let key_owner_name = CFString::from_static_string("kCGWindowOwnerName");
        for i in 0..count {
            let dict_ptr = CFArrayGetValueAtIndex(arr, i) as CFDictionaryRef;
            if dict_ptr.is_null() { continue; }
            let mut layer_i64: i64 = -1;
            let layer_ptr = CFDictionaryGetValue(dict_ptr, key_layer.as_concrete_TypeRef() as *const _);
            if !layer_ptr.is_null() { let _ = CFNumberGetValue(layer_ptr as CFNumberRef, kCFNumberSInt64Type, &mut layer_i64 as *mut _ as *mut _); }
            if layer_i64 != 0 { continue; }
            let mut pid_i64: i64 = 0;
            let pid_ptr = CFDictionaryGetValue(dict_ptr, key_pid.as_concrete_TypeRef() as *const _);
            if !pid_ptr.is_null() { let _ = CFNumberGetValue(pid_ptr as CFNumberRef, kCFNumberSInt64Type, &mut pid_i64 as *mut _ as *mut _); }
            let owner_name_ptr = CFDictionaryGetValue(dict_ptr, key_owner_name.as_concrete_TypeRef() as *const _);
            let owner_name = if !owner_name_ptr.is_null() { CFString::wrap_under_get_rule(owner_name_ptr as CFStringRef).to_string() } else { String::new() };
            let name_ptr = CFDictionaryGetValue(dict_ptr, key_name.as_concrete_TypeRef() as *const _);
            let title = if !name_ptr.is_null() { CFString::wrap_under_get_rule(name_ptr as CFStringRef).to_string() } else { String::new() };
            out.push(WindowInfoDto { owner_name, owner_pid: pid_i64, layer: layer_i64, window_title: title });
            if out.len() >= limit { break; }
        }
    }
    out
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn list_windows_debug(_limit: usize) -> Vec<WindowInfoDto> { Vec::new() }

#[cfg(target_os = "macos")]
unsafe fn nsstring_to_string(s: *mut objc::runtime::Object) -> String {
    use objc::{msg_send, sel, sel_impl};
    let bytes: *const std::os::raw::c_char = msg_send![s, UTF8String];
    if bytes.is_null() { return String::new(); }
    std::ffi::CStr::from_ptr(bytes).to_string_lossy().into_owned()
}

#[cfg(target_os = "macos")]
fn cg_seconds_since_last_input() -> f64 {
    // CGEventSourceSecondsSinceLastEventType(kCGEventSourceStateCombinedSessionState=0, kCGAnyInputEventType=-1)
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state_id: u32, event_type: i32) -> f64;
    }
    unsafe { CGEventSourceSecondsSinceLastEventType(0, -1) }
}

#[cfg(target_os = "macos")]
fn cg_front_window_title(owner_pid: i64) -> Option<String> {
    use core_foundation::string::CFString;
    use core_foundation::base::TCFType;
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
    use core_foundation_sys::number::{kCFNumberSInt64Type, CFNumberGetValue, CFNumberRef};
    use core_foundation_sys::string::CFStringRef;

    const K_ONSCREEN_ONLY: u32 = 1; // kCGWindowListOptionOnScreenOnly
    const K_EXCLUDE_DESKTOP: u32 = 16; // kCGWindowListExcludeDesktopElements
    extern "C" { fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> CFArrayRef; }

    unsafe {
        let arr = CGWindowListCopyWindowInfo(K_ONSCREEN_ONLY | K_EXCLUDE_DESKTOP, 0);
        if arr.is_null() { return None; }
        let count = CFArrayGetCount(arr);
        let key_pid = CFString::from_static_string("kCGWindowOwnerPID");
        let key_layer = CFString::from_static_string("kCGWindowLayer");
        let key_name = CFString::from_static_string("kCGWindowName");
        for i in 0..count {
            let dict_ptr = CFArrayGetValueAtIndex(arr, i) as CFDictionaryRef;
            if dict_ptr.is_null() { continue; }
            // pid
            let pid_ptr = CFDictionaryGetValue(dict_ptr, key_pid.as_concrete_TypeRef() as *const _);
            if pid_ptr.is_null() { continue; }
            let mut pid_i64: i64 = 0;
            let _ok = CFNumberGetValue(pid_ptr as CFNumberRef, kCFNumberSInt64Type, &mut pid_i64 as *mut _ as *mut _);
            if pid_i64 != owner_pid { continue; }
            // layer
            let layer_ptr = CFDictionaryGetValue(dict_ptr, key_layer.as_concrete_TypeRef() as *const _);
            if layer_ptr.is_null() { continue; }
            let mut layer_i64: i64 = -1;
            let _ok2 = CFNumberGetValue(layer_ptr as CFNumberRef, kCFNumberSInt64Type, &mut layer_i64 as *mut _ as *mut _);
            if layer_i64 != 0 { continue; }
            // name
            let name_ptr = CFDictionaryGetValue(dict_ptr, key_name.as_concrete_TypeRef() as *const _);
            if name_ptr.is_null() { continue; }
            let cfs = CFString::wrap_under_get_rule(name_ptr as CFStringRef);
            let s = cfs.to_string();
            if !s.is_empty() { return Some(s); }
        }
    }
    None
}

// Intenta obtener la app enfocada vía Accessibility (AXUIElementCreateSystemWide → AXFocusedApplication)
#[cfg(target_os = "macos")]
fn ax_focused_app() -> Option<(i32, String)> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFRelease, CFTypeRef};
    #[repr(C)]
    struct __AXUIElement;
    type AXUIElementRef = *mut __AXUIElement;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(element: AXUIElementRef, attr: core_foundation_sys::string::CFStringRef, value: *mut CFTypeRef) -> i32;
        fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> i32;
    }
    unsafe {
        let sys = AXUIElementCreateSystemWide();
        if sys.is_null() { return None; }
        // Preferir AXFocusedUIElement (más preciso en muchas configuraciones)
        let key_focused = CFString::from_static_string("AXFocusedUIElement");
        let mut elem_ref: CFTypeRef = std::ptr::null();
        let mut pid: i32 = 0;
        let mut ok = false;
        let err_elem = AXUIElementCopyAttributeValue(sys, key_focused.as_concrete_TypeRef(), &mut elem_ref);
        if err_elem == 0 && !elem_ref.is_null() {
            let el = elem_ref as AXUIElementRef;
            let _ = AXUIElementGetPid(el, &mut pid as *mut _);
            ok = pid != 0;
            CFRelease(elem_ref);
        }
        if !ok {
            // Fallback: AXFocusedApplication
            let key_app = CFString::from_static_string("AXFocusedApplication");
            let mut app_ref: CFTypeRef = std::ptr::null();
            let err_app = AXUIElementCopyAttributeValue(sys, key_app.as_concrete_TypeRef(), &mut app_ref);
            if err_app != 0 || app_ref.is_null() { return None; }
            let app_el = app_ref as AXUIElementRef;
            let _ = AXUIElementGetPid(app_el, &mut pid as *mut _);
            CFRelease(app_ref);
            if pid == 0 { return None; }
        }
        let name = ns_running_app_name(pid).unwrap_or_else(|| String::from("Unknown"));
        Some((pid, name))
    }
}

#[cfg(target_os = "macos")]
fn ns_running_app_name(pid: i32) -> Option<String> {
    use objc::{class, msg_send, sel, sel_impl};
    use objc::runtime::Object;
    unsafe {
        let nsapp: *mut Object = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if nsapp.is_null() { return None; }
        let name: *mut Object = msg_send![nsapp, localizedName];
        Some(nsstring_to_string(name))
    }
}

// Determina el primer owner/layer 0 del listado de ventanas y devuelve (owner_name, owner_pid, window_title?)
#[cfg(target_os = "macos")]
fn cg_front_window_owner_and_title() -> Option<(String, i64, Option<String>)> {
    use core_foundation::string::CFString;
    use core_foundation::base::TCFType;
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
    use core_foundation_sys::number::{kCFNumberSInt64Type, CFNumberGetValue, CFNumberRef};
    use core_foundation_sys::string::CFStringRef;

    const K_ONSCREEN_ONLY: u32 = 1; // kCGWindowListOptionOnScreenOnly
    const K_EXCLUDE_DESKTOP: u32 = 16; // kCGWindowListExcludeDesktopElements
    extern "C" { fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> CFArrayRef; }

    unsafe {
        let arr = CGWindowListCopyWindowInfo(K_ONSCREEN_ONLY | K_EXCLUDE_DESKTOP, 0);
        if arr.is_null() { return None; }
        let count = CFArrayGetCount(arr);
        let key_pid = CFString::from_static_string("kCGWindowOwnerPID");
        let key_layer = CFString::from_static_string("kCGWindowLayer");
        let key_name = CFString::from_static_string("kCGWindowName");
        let key_owner_name = CFString::from_static_string("kCGWindowOwnerName");
        for i in 0..count {
            let dict_ptr = CFArrayGetValueAtIndex(arr, i) as CFDictionaryRef;
            if dict_ptr.is_null() { continue; }
            // layer
            let layer_ptr = CFDictionaryGetValue(dict_ptr, key_layer.as_concrete_TypeRef() as *const _);
            if layer_ptr.is_null() { continue; }
            let mut layer_i64: i64 = -1;
            let _ok2 = CFNumberGetValue(layer_ptr as CFNumberRef, kCFNumberSInt64Type, &mut layer_i64 as *mut _ as *mut _);
            if layer_i64 != 0 { continue; }
            // owner pid
            let pid_ptr = CFDictionaryGetValue(dict_ptr, key_pid.as_concrete_TypeRef() as *const _);
            if pid_ptr.is_null() { continue; }
            let mut pid_i64: i64 = 0;
            let _ok = CFNumberGetValue(pid_ptr as CFNumberRef, kCFNumberSInt64Type, &mut pid_i64 as *mut _ as *mut _);
            // owner name
            let owner_name_ptr = CFDictionaryGetValue(dict_ptr, key_owner_name.as_concrete_TypeRef() as *const _);
            if owner_name_ptr.is_null() { continue; }
            let owner_cfs = CFString::wrap_under_get_rule(owner_name_ptr as CFStringRef);
            let owner_name = owner_cfs.to_string();
            // window title (puede ser nulo/empty)
            let name_ptr = CFDictionaryGetValue(dict_ptr, key_name.as_concrete_TypeRef() as *const _);
            let maybe_title = if name_ptr.is_null() {
                None
            } else {
                let cfs = CFString::wrap_under_get_rule(name_ptr as CFStringRef);
                let s = cfs.to_string();
                if s.is_empty() { None } else { Some(s) }
            };
            return Some((owner_name, pid_i64, maybe_title));
        }
    }
    None
}

// Fallback: usar Accessibility para obtener el título de la ventana enfocada.
#[cfg(target_os = "macos")]
fn ax_window_title(pid: i32) -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFRelease, CFTypeRef};
    use core_foundation_sys::string::CFStringRef;

    #[repr(C)]
    struct __AXUIElement;
    type AXUIElementRef = *mut __AXUIElement;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(element: AXUIElementRef, attr: CFStringRef, value: *mut CFTypeRef) -> i32; // AXError
    }

    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() { return None; }
        let k_focused = CFString::from_static_string("AXFocusedWindow");
        let mut win_ref: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, k_focused.as_concrete_TypeRef(), &mut win_ref);
        if err != 0 || win_ref.is_null() {
            return None;
        }
        let window: AXUIElementRef = win_ref as AXUIElementRef;
        let k_title = CFString::from_static_string("AXTitle");
        let mut title_ref: CFTypeRef = std::ptr::null();
        let err2 = AXUIElementCopyAttributeValue(window, k_title.as_concrete_TypeRef(), &mut title_ref);
        if err2 != 0 || title_ref.is_null() {
            // liberar referencia de ventana
            CFRelease(win_ref);
            return None;
        }
        let cfs = CFString::wrap_under_create_rule(title_ref as CFStringRef);
        let s = cfs.to_string();
        // liberar referencia de ventana
        CFRelease(win_ref);
        if s.is_empty() { None } else { Some(s) }
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

// Marca de diagnóstico para no inundar los logs
#[cfg(target_os = "macos")]
static PERMS_WARNED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
fn perms_diag_once() {
    if !PERMS_WARNED.swap(true, Ordering::Relaxed) {
        let perms = crate::macos_perms::check_permissions();
        warn!(?perms, "No se pudo obtener el título de la ventana (probables permisos faltantes). Visita /permissions para estado o /permissions/prompt para solicitar.");
        println!(
            "[hint] Títulos vacíos: permisos macOS. Revisa http://127.0.0.1:49219/permissions y usa http://127.0.0.1:49219/permissions/prompt"
        );
    }
}
