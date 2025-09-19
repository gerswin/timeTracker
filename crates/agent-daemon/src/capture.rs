use agent_core::queue::Queue;
use agent_core::state::AgentState;
use anyhow::Result;
use serde::Serialize;
use crate::policy::{PolicyRuntime, PolicyState};
use globset::{Glob, GlobSetBuilder};
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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
    policy_rt: std::sync::Arc<PolicyRuntime>,
    dropped_counter: Arc<AtomicU64>,
    drop_counters: Arc<crate::policy::DropCounters>,
) {
    info!("iniciando loop de captura (Fase 1)");
    println!("[debug] capture loop started");
    let mut prev_app = String::new();
    let mut prev_title = String::new();
    // Throttle state
    let mut thr = Throttle::new();
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
                // Apply policy filters
                let pol = policy_rt.get();
                thr.update_from_policy(&pol.policy);
                if let Some(reason) = drop_reason(&pol, &app, &title) {
                    dropped_counter.fetch_add(1, Ordering::Relaxed);
                    match reason {
                        DropReason::KillSwitch => drop_counters.kill_switch.fetch_add(1, Ordering::Relaxed),
                        DropReason::PauseCapture => drop_counters.pause.fetch_add(1, Ordering::Relaxed),
                        DropReason::ExcludedApp => drop_counters.excluded_app.fetch_add(1, Ordering::Relaxed),
                        DropReason::ExcludedPattern => drop_counters.excluded_pattern.fetch_add(1, Ordering::Relaxed),
                        DropReason::Throttled => drop_counters.throttled.fetch_add(1, Ordering::Relaxed),
                    };
                    sleep(Duration::from_millis(1000)).await;
                    continue;
                }
                let effective_title = if pol.policy.titleCapture { title.clone() } else { String::new() };
                // Emitir solo en cambio o cada 30s
                let changed = app != prev_app || effective_title != prev_title;
                let force_emit = should_force_emit(last_event_ts.load(Ordering::Relaxed));
                if changed || force_emit {
                    if !thr.permit(now) {
                        dropped_counter.fetch_add(1, Ordering::Relaxed);
                        drop_counters.throttled.fetch_add(1, Ordering::Relaxed);
                        // Throttled: no emit this tick
                        sleep(Duration::from_millis(1000)).await;
                        continue;
                    }
                    let evt = CaptureEvent {
                        ts_ms: now_ms(),
                        app_name: app.clone(),
                        window_title: effective_title.clone(),
                        input_idle_ms: idle_ms,
                    };
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
                    prev_title = effective_title;
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
    if last_ts == 0 {
        return true;
    }
    let now = now_ms();
    now.saturating_sub(last_ts) > 30_000
}

#[derive(Copy, Clone)]
enum DropReason { KillSwitch, PauseCapture, ExcludedApp, ExcludedPattern, Throttled }

fn drop_reason(pol: &PolicyState, app: &str, title: &str) -> Option<DropReason> {
    let p = &pol.policy;
    if p.killSwitch { return Some(DropReason::KillSwitch); }
    if p.pauseCapture { return Some(DropReason::PauseCapture); }
    if !p.excludeApps.is_empty() && p.excludeApps.iter().any(|a| a == app) { return Some(DropReason::ExcludedApp); }
    if !p.excludePatterns.is_empty() {
        let mut b = GlobSetBuilder::new();
        for pat in &p.excludePatterns {
            if let Ok(g) = Glob::new(pat) { b.add(g); }
        }
        if let Ok(gs) = b.build() { if gs.is_match(title) { return Some(DropReason::ExcludedPattern); } }
    }
    None
}

struct Throttle {
    capacity: f64,
    tokens: f64,
    rate_per_sec: f64,
    last_refill_ms: u64,
    min_interval_ms: u64,
    last_emit_ms: u64,
}

impl Throttle {
    fn new() -> Self {
        let mut t = Self { capacity: 10.0, tokens: 10.0, rate_per_sec: 10.0/60.0, last_refill_ms: now_ms(), min_interval_ms: 500, last_emit_ms: 0 };
        t
    }
    fn update_from_policy(&mut self, pol: &crate::policy::Policy) {
        if let Some(bpm) = pol.titleBurstPerMinute { let cap = bpm.max(1) as f64; self.capacity = cap; self.rate_per_sec = cap/60.0; if self.tokens > self.capacity { self.tokens = self.capacity; } }
        if let Some(hz) = pol.titleSampleHz { let hz = hz.max(1) as u64; self.min_interval_ms = (1000 / hz).max(100); }
    }
    fn refill(&mut self, now: u64) {
        let dt_ms = now.saturating_sub(self.last_refill_ms);
        if dt_ms == 0 { return; }
        let add = self.rate_per_sec * (dt_ms as f64)/1000.0;
        self.tokens = (self.tokens + add).min(self.capacity);
        self.last_refill_ms = now;
    }
    fn permit(&mut self, now: u64) -> bool {
        self.refill(now);
        if self.last_emit_ms != 0 && now.saturating_sub(self.last_emit_ms) < self.min_interval_ms { return false; }
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            self.last_emit_ms = now;
            true
        } else { false }
    }
}

#[cfg(target_os = "macos")]
fn sample_once() -> Result<(String, String, u64)> {
    // 0) Preferir AX sistema: app enfocada (más fiable entre Spaces)
    if let Some((ax_pid, ax_name)) = ax_focused_app() {
        let title = cg_front_window_title(ax_pid as i64)
            .or_else(|| ax_window_title(ax_pid))
            .unwrap_or_default();
        if title.is_empty() {
            perms_diag_once();
        }
        let idle_ms = (cg_seconds_since_last_input() * 1000.0).round() as u64;
        return Ok((ax_name, title, idle_ms));
    }
    // 1) CoreGraphics: ventana top (layer 0) → owner y título
    if let Some((owner_name, owner_pid, maybe_title)) = cg_front_window_owner_and_title() {
        let title = maybe_title
            .or_else(|| ax_window_title(owner_pid as i32))
            .unwrap_or_default();
        if title.is_empty() {
            perms_diag_once();
        }
        let idle_ms = (cg_seconds_since_last_input() * 1000.0).round() as u64;
        return Ok((owner_name, title, idle_ms));
    }
    // 2) Fallback: NSWorkspace + AX (si CG no devolvió nada)
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() {
            return Ok((String::new(), String::new(), 0));
        }
        let app: *mut Object = msg_send![ws, frontmostApplication];
        if app.is_null() {
            return Ok((String::new(), String::new(), 0));
        }
        let name: *mut Object = msg_send![app, localizedName];
        let app_name = nsstring_to_string(name);
        let pid: i32 = msg_send![app, processIdentifier];
        let title = ax_window_title(pid).unwrap_or_default();
        if title.is_empty() {
            perms_diag_once();
        }
        let idle_ms = (cg_seconds_since_last_input() * 1000.0).round() as u64;
        Ok((app_name, title, idle_ms))
    }
}

#[cfg(target_os = "windows")]
fn sample_once() -> Result<(String, String, u64)> {
    let snapshot = capture_foreground()?;
    let idle_ms = windows_idle_ms();
    Ok((snapshot.app_name, snapshot.window_title, idle_ms))
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
    #[cfg(target_os = "windows")]
    pub win_pid: Option<u32>,
    #[cfg(target_os = "windows")]
    pub win_thread_id: Option<u32>,
    #[cfg(target_os = "windows")]
    pub win_hwnd: Option<String>,
    #[cfg(target_os = "windows")]
    pub win_root_hwnd: Option<String>,
    #[cfg(target_os = "windows")]
    pub win_class: Option<String>,
    #[cfg(target_os = "windows")]
    pub win_process_path: Option<String>,

    #[cfg(target_os = "macos")]
    pub perms: super::macos_perms::PermsStatus,
}

#[cfg(target_os = "macos")]
pub fn sample_debug() -> Result<SampleDebugDto> {
    // Triangulación: AX (preferente), luego NS, luego CG
    let ax = ax_focused_app();
    let ns = unsafe {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() {
            None
        } else {
            let app: *mut Object = msg_send![ws, frontmostApplication];
            if app.is_null() {
                None
            } else {
                let name: *mut Object = msg_send![app, localizedName];
                let app_name = nsstring_to_string(name);
                let pid: i32 = msg_send![app, processIdentifier];
                Some((pid, app_name))
            }
        }
    };
    let cg = cg_front_window_owner_and_title();

    // Efectivo: elegir PID/nombre priorizando AX → NS → CG
    let (eff_pid, eff_name) = if let Some((p, n)) = &ax {
        (*p, n.clone())
    } else if let Some((p, n)) = &ns {
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
        ax_pid: ax.as_ref().map(|(p, _)| *p),
        ax_name: ax.as_ref().map(|(_, n)| n.clone()),
        ns_pid: ns.as_ref().map(|(p, _)| *p),
        ns_name: ns.as_ref().map(|(_, n)| n.clone()),
        cg_pid: cg.as_ref().map(|(_, p, _)| *p),
        cg_owner: cg.as_ref().map(|(o, _, _)| o.clone()),
        cg_title: cg_front_window_title(eff_pid as i64),
        ax_title: ax_window_title(eff_pid),
        perms: super::macos_perms::check_permissions(),
    })
}

#[cfg(target_os = "windows")]
pub fn sample_debug() -> Result<SampleDebugDto> {
    let snapshot = capture_foreground()?;
    let idle = windows_idle_ms();
    Ok(SampleDebugDto {
        app_name: snapshot.app_name.clone(),
        window_title: snapshot.window_title.clone(),
        input_idle_ms: idle,
        title_source: snapshot.strategy.clone(),
        ax_pid: None,
        ax_name: None,
        ns_pid: None,
        ns_name: None,
        cg_pid: None,
        cg_owner: None,
        cg_title: None,
        ax_title: None,
        #[cfg(target_os = "windows")]
        win_pid: Some(snapshot.pid),
        #[cfg(target_os = "windows")]
        win_thread_id: Some(snapshot.thread_id),
        #[cfg(target_os = "windows")]
        win_hwnd: Some(format!("0x{:X}", snapshot.active_hwnd.0 as isize as usize)),
        #[cfg(target_os = "windows")]
        win_root_hwnd: Some(format!(
            "0x{:X}",
            snapshot.top_level_hwnd.0 as isize as usize
        )),
        #[cfg(target_os = "windows")]
        win_class: Some(snapshot.class_name.clone()),
        #[cfg(target_os = "windows")]
        win_process_path: snapshot.process_path.clone(),
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
struct WinForegroundSnapshot {
    active_hwnd: windows::Win32::Foundation::HWND,
    top_level_hwnd: windows::Win32::Foundation::HWND,
    pid: u32,
    thread_id: u32,
    app_name: String,
    window_title: String,
    class_name: String,
    strategy: String,
    process_path: Option<String>,
}

#[cfg(target_os = "windows")]
fn capture_foreground() -> Result<WinForegroundSnapshot> {
    use anyhow::bail;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
        IsWindowVisible, GA_ROOT, GUITHREADINFO,
    };

    let mut strategy = String::from("GetForegroundWindow");
    unsafe {
        let mut active_hwnd = GetForegroundWindow();
        let mut gui_info: Option<GUITHREADINFO> = None;

        let mut gui: GUITHREADINFO = std::mem::zeroed();
        gui.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        if GetGUIThreadInfo(0, &mut gui).is_ok() {
            gui_info = Some(gui);
            if active_hwnd.0 == 0 {
                if gui.hwndFocus.0 != 0 {
                    active_hwnd = gui.hwndFocus;
                    strategy = "GetGUIThreadInfo::hwndFocus".to_string();
                } else if gui.hwndActive.0 != 0 {
                    active_hwnd = gui.hwndActive;
                    strategy = "GetGUIThreadInfo::hwndActive".to_string();
                } else if gui.hwndCapture.0 != 0 {
                    active_hwnd = gui.hwndCapture;
                    strategy = "GetGUIThreadInfo::hwndCapture".to_string();
                } else if gui.hwndCaret.0 != 0 {
                    active_hwnd = gui.hwndCaret;
                    strategy = "GetGUIThreadInfo::hwndCaret".to_string();
                }
            }
        }

        if active_hwnd.0 == 0 {
            bail!("no se pudo obtener la ventana activa");
        }

        let mut top_level_hwnd = GetAncestor(active_hwnd, GA_ROOT);
        if top_level_hwnd.0 == 0 {
            top_level_hwnd = active_hwnd;
        } else if top_level_hwnd != active_hwnd {
            strategy.push_str("->GA_ROOT");
        }

        if !IsWindowVisible(top_level_hwnd).as_bool() && IsWindowVisible(active_hwnd).as_bool() {
            top_level_hwnd = active_hwnd;
            strategy.push_str("+visible-active");
        }

        let mut pid: u32 = 0;
        let thread_id = GetWindowThreadProcessId(top_level_hwnd, Some(&mut pid));
        if pid == 0 {
            bail!("no se pudo resolver el PID de la ventana activa");
        }

        let mut window_title = read_window_text(top_level_hwnd);
        let mut fallbacks: Vec<(HWND, &str)> = Vec::new();
        if let Some(gui) = gui_info {
            if gui.hwndFocus.0 != 0 {
                fallbacks.push((gui.hwndFocus, "hwndFocus"));
            }
            if gui.hwndActive.0 != 0 {
                fallbacks.push((gui.hwndActive, "hwndActive"));
            }
            if gui.hwndCaret.0 != 0 {
                fallbacks.push((gui.hwndCaret, "hwndCaret"));
            }
        }
        if active_hwnd != top_level_hwnd {
            fallbacks.push((active_hwnd, "foreground"));
        }
        fallbacks.push((top_level_hwnd, "topLevel"));

        if window_title.trim().is_empty() {
            for (candidate, label) in fallbacks.iter() {
                if candidate.0 == 0 {
                    continue;
                }
                let alt = read_window_text(*candidate);
                if !alt.trim().is_empty() {
                    window_title = alt;
                    strategy.push_str(&format!("+{}", label));
                    break;
                }
            }
        }

        let class_name = read_class_name(top_level_hwnd);
        let proc_info = process_info_from_pid(pid);

        Ok(WinForegroundSnapshot {
            active_hwnd,
            top_level_hwnd,
            pid,
            thread_id,
            app_name: proc_info.name,
            window_title,
            class_name,
            strategy,
            process_path: proc_info.exe,
        })
    }
}

#[cfg(target_os = "windows")]
fn read_window_text(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};

    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        // Add some slack to avoid truncation when titles change between calls.
        let mut buf = vec![0u16; len.saturating_add(2) as usize + 64];
        let written = GetWindowTextW(hwnd, &mut buf);
        if written > 0 {
            String::from_utf16_lossy(&buf[..written as usize])
        } else {
            String::new()
        }
    }
}

#[cfg(target_os = "windows")]
fn read_class_name(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;

    unsafe {
        let mut buf = vec![0u16; 128];
        let written = GetClassNameW(hwnd, &mut buf);
        if written > 0 {
            String::from_utf16_lossy(&buf[..written as usize])
        } else {
            String::new()
        }
    }
}

#[cfg(target_os = "windows")]
struct ProcessInfo {
    name: String,
    exe: Option<String>,
}

#[cfg(target_os = "windows")]
fn process_info_from_pid(pid: u32) -> ProcessInfo {
    let mut sys = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    sys.refresh_process(pid);
    if let Some(proc) = sys.process(pid) {
        let exe = proc.exe().map(|p| p.to_string_lossy().into_owned());
        ProcessInfo {
            name: proc.name().to_string(),
            exe,
        }
    } else {
        ProcessInfo {
            name: "Unknown".to_string(),
            exe: None,
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_idle_ms() -> u64 {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetLastInputInfo;
    use windows::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO;
    unsafe {
        let mut lii = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut lii).as_bool() {
            let now = GetTickCount();
            return now.wrapping_sub(lii.dwTime) as u64;
        }
    }
    0
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
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() {
            None
        } else {
            let app: *mut Object = msg_send![ws, frontmostApplication];
            if app.is_null() {
                None
            } else {
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
        ax_pid: ax.as_ref().map(|(p, _)| *p),
        ax_name: ax.as_ref().map(|(_, n)| n.clone()),
        ns_pid: ns.as_ref().map(|(p, _)| *p),
        ns_name: ns.as_ref().map(|(_, n)| n.clone()),
        cg_pid: cg.as_ref().map(|(_, p, _)| *p),
        cg_owner: cg.as_ref().map(|(o, _, _)| o.clone()),
        cg_title: cg.as_ref().and_then(|(_, _, t)| t.clone()),
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn frontmost_debug() -> FrontmostDebugDto {
    FrontmostDebugDto {
        ax_pid: None,
        ax_name: None,
        ns_pid: None,
        ns_name: None,
        cg_pid: None,
        cg_owner: None,
        cg_title: None,
    }
}

#[cfg(target_os = "macos")]
pub fn list_windows_debug(limit: usize) -> Vec<WindowInfoDto> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
    use core_foundation_sys::number::{kCFNumberSInt64Type, CFNumberGetValue, CFNumberRef};
    use core_foundation_sys::string::CFStringRef;

    const K_ONSCREEN_ONLY: u32 = 1; // kCGWindowListOptionOnScreenOnly
    const K_EXCLUDE_DESKTOP: u32 = 16; // kCGWindowListExcludeDesktopElements
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> CFArrayRef;
    }

    let mut out = Vec::new();
    unsafe {
        let arr = CGWindowListCopyWindowInfo(K_ONSCREEN_ONLY | K_EXCLUDE_DESKTOP, 0);
        if arr.is_null() {
            return out;
        }
        let count = CFArrayGetCount(arr);
        let key_pid = CFString::from_static_string("kCGWindowOwnerPID");
        let key_layer = CFString::from_static_string("kCGWindowLayer");
        let key_name = CFString::from_static_string("kCGWindowName");
        let key_owner_name = CFString::from_static_string("kCGWindowOwnerName");
        for i in 0..count {
            let dict_ptr = CFArrayGetValueAtIndex(arr, i) as CFDictionaryRef;
            if dict_ptr.is_null() {
                continue;
            }
            let mut layer_i64: i64 = -1;
            let layer_ptr =
                CFDictionaryGetValue(dict_ptr, key_layer.as_concrete_TypeRef() as *const _);
            if !layer_ptr.is_null() {
                let _ = CFNumberGetValue(
                    layer_ptr as CFNumberRef,
                    kCFNumberSInt64Type,
                    &mut layer_i64 as *mut _ as *mut _,
                );
            }
            if layer_i64 != 0 {
                continue;
            }
            let mut pid_i64: i64 = 0;
            let pid_ptr = CFDictionaryGetValue(dict_ptr, key_pid.as_concrete_TypeRef() as *const _);
            if !pid_ptr.is_null() {
                let _ = CFNumberGetValue(
                    pid_ptr as CFNumberRef,
                    kCFNumberSInt64Type,
                    &mut pid_i64 as *mut _ as *mut _,
                );
            }
            let owner_name_ptr =
                CFDictionaryGetValue(dict_ptr, key_owner_name.as_concrete_TypeRef() as *const _);
            let owner_name = if !owner_name_ptr.is_null() {
                CFString::wrap_under_get_rule(owner_name_ptr as CFStringRef).to_string()
            } else {
                String::new()
            };
            let name_ptr =
                CFDictionaryGetValue(dict_ptr, key_name.as_concrete_TypeRef() as *const _);
            let title = if !name_ptr.is_null() {
                CFString::wrap_under_get_rule(name_ptr as CFStringRef).to_string()
            } else {
                String::new()
            };
            out.push(WindowInfoDto {
                owner_name,
                owner_pid: pid_i64,
                layer: layer_i64,
                window_title: title,
            });
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn list_windows_debug(_limit: usize) -> Vec<WindowInfoDto> {
    Vec::new()
}

#[cfg(target_os = "macos")]
unsafe fn nsstring_to_string(s: *mut objc::runtime::Object) -> String {
    use objc::{msg_send, sel, sel_impl};
    let bytes: *const std::os::raw::c_char = msg_send![s, UTF8String];
    if bytes.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(bytes)
        .to_string_lossy()
        .into_owned()
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
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
    use core_foundation_sys::number::{kCFNumberSInt64Type, CFNumberGetValue, CFNumberRef};
    use core_foundation_sys::string::CFStringRef;

    const K_ONSCREEN_ONLY: u32 = 1; // kCGWindowListOptionOnScreenOnly
    const K_EXCLUDE_DESKTOP: u32 = 16; // kCGWindowListExcludeDesktopElements
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> CFArrayRef;
    }

    unsafe {
        let arr = CGWindowListCopyWindowInfo(K_ONSCREEN_ONLY | K_EXCLUDE_DESKTOP, 0);
        if arr.is_null() {
            return None;
        }
        let count = CFArrayGetCount(arr);
        let key_pid = CFString::from_static_string("kCGWindowOwnerPID");
        let key_layer = CFString::from_static_string("kCGWindowLayer");
        let key_name = CFString::from_static_string("kCGWindowName");
        for i in 0..count {
            let dict_ptr = CFArrayGetValueAtIndex(arr, i) as CFDictionaryRef;
            if dict_ptr.is_null() {
                continue;
            }
            // pid
            let pid_ptr = CFDictionaryGetValue(dict_ptr, key_pid.as_concrete_TypeRef() as *const _);
            if pid_ptr.is_null() {
                continue;
            }
            let mut pid_i64: i64 = 0;
            let _ok = CFNumberGetValue(
                pid_ptr as CFNumberRef,
                kCFNumberSInt64Type,
                &mut pid_i64 as *mut _ as *mut _,
            );
            if pid_i64 != owner_pid {
                continue;
            }
            // layer
            let layer_ptr =
                CFDictionaryGetValue(dict_ptr, key_layer.as_concrete_TypeRef() as *const _);
            if layer_ptr.is_null() {
                continue;
            }
            let mut layer_i64: i64 = -1;
            let _ok2 = CFNumberGetValue(
                layer_ptr as CFNumberRef,
                kCFNumberSInt64Type,
                &mut layer_i64 as *mut _ as *mut _,
            );
            if layer_i64 != 0 {
                continue;
            }
            // name
            let name_ptr =
                CFDictionaryGetValue(dict_ptr, key_name.as_concrete_TypeRef() as *const _);
            if name_ptr.is_null() {
                continue;
            }
            let cfs = CFString::wrap_under_get_rule(name_ptr as CFStringRef);
            let s = cfs.to_string();
            if !s.is_empty() {
                return Some(s);
            }
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
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attr: core_foundation_sys::string::CFStringRef,
            value: *mut CFTypeRef,
        ) -> i32;
        fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> i32;
    }
    unsafe {
        let sys = AXUIElementCreateSystemWide();
        if sys.is_null() {
            return None;
        }
        // Preferir AXFocusedUIElement (más preciso en muchas configuraciones)
        let key_focused = CFString::from_static_string("AXFocusedUIElement");
        let mut elem_ref: CFTypeRef = std::ptr::null();
        let mut pid: i32 = 0;
        let mut ok = false;
        let err_elem =
            AXUIElementCopyAttributeValue(sys, key_focused.as_concrete_TypeRef(), &mut elem_ref);
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
            let err_app =
                AXUIElementCopyAttributeValue(sys, key_app.as_concrete_TypeRef(), &mut app_ref);
            if err_app != 0 || app_ref.is_null() {
                return None;
            }
            let app_el = app_ref as AXUIElementRef;
            let _ = AXUIElementGetPid(app_el, &mut pid as *mut _);
            CFRelease(app_ref);
            if pid == 0 {
                return None;
            }
        }
        let name = ns_running_app_name(pid).unwrap_or_else(|| String::from("Unknown"));
        Some((pid, name))
    }
}

#[cfg(target_os = "macos")]
fn ns_running_app_name(pid: i32) -> Option<String> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let nsapp: *mut Object =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if nsapp.is_null() {
            return None;
        }
        let name: *mut Object = msg_send![nsapp, localizedName];
        Some(nsstring_to_string(name))
    }
}

// Determina el primer owner/layer 0 del listado de ventanas y devuelve (owner_name, owner_pid, window_title?)
#[cfg(target_os = "macos")]
fn cg_front_window_owner_and_title() -> Option<(String, i64, Option<String>)> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
    use core_foundation_sys::number::{kCFNumberSInt64Type, CFNumberGetValue, CFNumberRef};
    use core_foundation_sys::string::CFStringRef;

    const K_ONSCREEN_ONLY: u32 = 1; // kCGWindowListOptionOnScreenOnly
    const K_EXCLUDE_DESKTOP: u32 = 16; // kCGWindowListExcludeDesktopElements
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> CFArrayRef;
    }

    unsafe {
        let arr = CGWindowListCopyWindowInfo(K_ONSCREEN_ONLY | K_EXCLUDE_DESKTOP, 0);
        if arr.is_null() {
            return None;
        }
        let count = CFArrayGetCount(arr);
        let key_pid = CFString::from_static_string("kCGWindowOwnerPID");
        let key_layer = CFString::from_static_string("kCGWindowLayer");
        let key_name = CFString::from_static_string("kCGWindowName");
        let key_owner_name = CFString::from_static_string("kCGWindowOwnerName");
        for i in 0..count {
            let dict_ptr = CFArrayGetValueAtIndex(arr, i) as CFDictionaryRef;
            if dict_ptr.is_null() {
                continue;
            }
            // layer
            let layer_ptr =
                CFDictionaryGetValue(dict_ptr, key_layer.as_concrete_TypeRef() as *const _);
            if layer_ptr.is_null() {
                continue;
            }
            let mut layer_i64: i64 = -1;
            let _ok2 = CFNumberGetValue(
                layer_ptr as CFNumberRef,
                kCFNumberSInt64Type,
                &mut layer_i64 as *mut _ as *mut _,
            );
            if layer_i64 != 0 {
                continue;
            }
            // owner pid
            let pid_ptr = CFDictionaryGetValue(dict_ptr, key_pid.as_concrete_TypeRef() as *const _);
            if pid_ptr.is_null() {
                continue;
            }
            let mut pid_i64: i64 = 0;
            let _ok = CFNumberGetValue(
                pid_ptr as CFNumberRef,
                kCFNumberSInt64Type,
                &mut pid_i64 as *mut _ as *mut _,
            );
            // owner name
            let owner_name_ptr =
                CFDictionaryGetValue(dict_ptr, key_owner_name.as_concrete_TypeRef() as *const _);
            if owner_name_ptr.is_null() {
                continue;
            }
            let owner_cfs = CFString::wrap_under_get_rule(owner_name_ptr as CFStringRef);
            let owner_name = owner_cfs.to_string();
            // window title (puede ser nulo/empty)
            let name_ptr =
                CFDictionaryGetValue(dict_ptr, key_name.as_concrete_TypeRef() as *const _);
            let maybe_title = if name_ptr.is_null() {
                None
            } else {
                let cfs = CFString::wrap_under_get_rule(name_ptr as CFStringRef);
                let s = cfs.to_string();
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
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
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attr: CFStringRef,
            value: *mut CFTypeRef,
        ) -> i32; // AXError
    }

    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        let k_focused = CFString::from_static_string("AXFocusedWindow");
        let mut win_ref: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, k_focused.as_concrete_TypeRef(), &mut win_ref);
        if err != 0 || win_ref.is_null() {
            return None;
        }
        let window: AXUIElementRef = win_ref as AXUIElementRef;
        let k_title = CFString::from_static_string("AXTitle");
        let mut title_ref: CFTypeRef = std::ptr::null();
        let err2 =
            AXUIElementCopyAttributeValue(window, k_title.as_concrete_TypeRef(), &mut title_ref);
        if err2 != 0 || title_ref.is_null() {
            // liberar referencia de ventana
            CFRelease(win_ref);
            return None;
        }
        let cfs = CFString::wrap_under_create_rule(title_ref as CFStringRef);
        let s = cfs.to_string();
        // liberar referencia de ventana
        CFRelease(win_ref);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
