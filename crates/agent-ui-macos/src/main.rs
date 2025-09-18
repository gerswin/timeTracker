#![allow(non_snake_case)]

#[cfg(target_os = "macos")]
mod app {
    use cocoa::appkit::{NSApp, NSApplication, NSApplicationActivateIgnoringOtherApps, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSWindow, NSControl};
    use cocoa::base::{id, nil, YES, NO};
    use cocoa::foundation::{NSAutoreleasePool, NSInteger, NSString, NSURL};
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::*;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    static mut HANDLER_SINGLETON: Option<Arc<Mutex<HandlerState>>> = None;

    struct HandlerState {
        status_item: id,
        panel_url: String,
        api_base: String,
        perm_ax_item: id,
        perm_sc_item: id,
        login_toggle_item: id,
    }

    pub fn run() {
        unsafe {
            let _pool = NSAutoreleasePool::new(nil);
            // Optional one-shot CLI to print Login Item state for testing
            let args: Vec<String> = std::env::args().collect();
            if args.iter().any(|a| a == "--print-login-state") {
                use std::ffi::CString;
                let c = CString::new("com.ripor.Ripor.LoginItem").unwrap();
                let sm = ripor_loginitem_is_registered(c.as_ptr());
                let la = is_login_enabled();
                println!("{{\"sm_registered\":{},\"launchagent_present\":{}}}", sm, la);
                return;
            }
            let app = NSApp();
            app.setActivationPolicy_(cocoa::appkit::NSApplicationActivationPolicy::NSApplicationActivationPolicyAccessory);

            // Ensure agent-daemon is running; if not, try to spawn from bundle resources
            ensure_agent_running();

            let status_bar = NSStatusBar::systemStatusBar(nil);
            let status_item: id = status_bar.statusItemWithLength_(cocoa::appkit::NSVariableStatusItemLength);
            // Try to assign template icon from Resources/iconTemplate.png
            if let Some(p) = bundle_icon_path() {
                let ns_path = NSString::alloc(nil).init_str(&p);
                let image: id = msg_send![class!(NSImage), alloc];
                let image: id = msg_send![image, initWithContentsOfFile: ns_path];
                if image != nil { let _: () = msg_send![image, setTemplate: YES]; let _: () = msg_send![status_item, setImage: image]; }
                else { let title = NSString::alloc(nil).init_str("Ripor ⦿"); status_item.setTitle_(title); }
            } else {
                let title = NSString::alloc(nil).init_str("Ripor ⦿"); status_item.setTitle_(title);
            }

            // Build menu
            let menu: id = NSMenu::new(nil).autorelease();
            let open_panel = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Ver política"),
                sel!(onOpenPanel:),
                NSString::alloc(nil).init_str(""),
            ).autorelease();
            menu.addItem_(open_panel);

            // Permission status items (read-only, refreshed in background)
            let perm_ax = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Accesibilidad: —"),
                sel!(noop:),
                NSString::alloc(nil).init_str(""),
            ).autorelease();
            perm_ax.setEnabled_(NO);
            menu.addItem_(perm_ax);

            let perm_sc = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Screen Recording: —"),
                sel!(noop:),
                NSString::alloc(nil).init_str(""),
            ).autorelease();
            perm_sc.setEnabled_(NO);
            menu.addItem_(perm_sc);

            // Quick actions to open Settings panes
            let open_ax = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Abrir Accesibilidad"),
                sel!(onOpenAx:),
                NSString::alloc(nil).init_str(""),
            ).autorelease();
            menu.addItem_(open_ax);

            let open_sc = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Abrir Screen Recording"),
                sel!(onOpenSc:),
                NSString::alloc(nil).init_str(""),
            ).autorelease();
            menu.addItem_(open_sc);

            menu.addItem_(NSMenuItem::separatorItem(nil));

            let pause15 = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Pausar 15 min"),
                sel!(onPause15:),
                NSString::alloc(nil).init_str(""),
            ).autorelease();
            menu.addItem_(pause15);

            let pause60 = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Pausar 60 min"),
                sel!(onPause60:),
                NSString::alloc(nil).init_str(""),
            ).autorelease();
            menu.addItem_(pause60);

            let resume = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Reanudar"),
                sel!(onResume:),
                NSString::alloc(nil).init_str(""),
            ).autorelease();
            menu.addItem_(resume);

            menu.addItem_(NSMenuItem::separatorItem(nil));
            // Login Item toggle (LaunchAgent fallback)
            let login_toggle = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Iniciar al abrir sesión"),
                sel!(onToggleLogin:),
                NSString::alloc(nil).init_str(""),
            ).autorelease();
            menu.addItem_(login_toggle);

            let quit = NSMenuItem::alloc(nil).initWithTitle_action_keyEquivalent_(
                NSString::alloc(nil).init_str("Salir"),
                sel!(onQuit:),
                NSString::alloc(nil).init_str("q"),
            ).autorelease();
            menu.addItem_(quit);

            status_item.setMenu_(menu);

            // Install handler class
            let handler_class = register_handler_class();
            let handler: id = msg_send![handler_class, new];
            let _: () = msg_send![menu, setAutoenablesItems: NO];
            open_panel.setTarget_(handler);
            open_ax.setTarget_(handler);
            open_sc.setTarget_(handler);
            pause15.setTarget_(handler);
            pause60.setTarget_(handler);
            resume.setTarget_(handler);
            login_toggle.setTarget_(handler);
            quit.setTarget_(handler);

            // Save state
            let api_base = std::env::var("PANEL_ADDR").unwrap_or_else(|_| "127.0.0.1:49219".to_string());
            let base = format!("http://{}", api_base);
            HANDLER_SINGLETON = Some(Arc::new(Mutex::new(HandlerState {
                status_item,
                panel_url: format!("{}/ui", base),
                api_base: base,
                perm_ax_item: perm_ax,
                perm_sc_item: perm_sc,
                login_toggle_item: login_toggle,
            })));

            // Background thread to refresh status (active/paused) and login toggle
            thread::spawn(|| loop {
                refresh_status_title();
                // Prefer SMAppService readback; fallback to LaunchAgent presence
                let (login_item, enabled) = unsafe {
                    match &HANDLER_SINGLETON {
                        Some(a) => {
                            let g = a.lock().unwrap();
                            let sm_enabled = {
                                use std::ffi::CString;
                                let c = CString::new("com.ripor.Ripor.LoginItem").unwrap();
                                ripor_loginitem_is_registered(c.as_ptr())
                            };
                            (g.login_toggle_item, if sm_enabled { true } else { is_login_enabled() })
                        }
                        None => (nil, false),
                    }
                };
                if login_item != nil {
                    unsafe {
                        let state: NSInteger = if enabled {1} else {0};
                        let _: () = msg_send![login_item, setState: state];
                    }
                }
                thread::sleep(Duration::from_secs(5));
            });

            app.activateIgnoringOtherApps_(YES);
            app.run();
        }
    }

    fn refresh_status_title() {
        let (status_item, api_base, perm_ax, perm_sc) = unsafe {
            match &HANDLER_SINGLETON {
                Some(a) => {
                    let g = a.lock().unwrap();
                    (g.status_item, g.api_base.clone(), g.perm_ax_item, g.perm_sc_item)
                }
                None => return,
            }
        };
        let url = format!("{}/state", api_base);
        let paused = match reqwest::blocking::get(&url) {
            Ok(resp) => {
                if let Ok(v) = resp.json::<serde_json::Value>() {
                    v.get("paused_until_ms").and_then(|x| x.as_u64()).unwrap_or(0) > 0
                } else { false }
            }
            Err(_) => false,
        };
        unsafe {
            // If there is no image assigned, use title fallback
            let img: id = msg_send![status_item, image];
            if img == nil {
                let t = if paused { "Ripor ⏸" } else { "Ripor ⦿" };
                let ns = NSString::alloc(nil).init_str(t);
                status_item.setTitle_(ns);
            }
        }

        // Refresh permission items
        let perms_url = format!("{}/permissions", api_base);
        if let Ok(resp) = reqwest::blocking::get(&perms_url) {
            if let Ok(v) = resp.json::<serde_json::Value>() {
                let ax_ok = v.get("accessibility_ok").and_then(|x| x.as_bool()).unwrap_or(false);
                let sc_ok = v.get("screen_recording_ok").and_then(|x| x.as_bool()).unwrap_or(false);
                unsafe {
                    let ax_t = if ax_ok { "Accesibilidad: OK" } else { "Accesibilidad: NO" };
                    let sc_t = if sc_ok { "Screen Recording: OK" } else { "Screen Recording: NO" };
                    let ax_ns = NSString::alloc(nil).init_str(ax_t);
                    let sc_ns = NSString::alloc(nil).init_str(sc_t);
                    perm_ax.setTitle_(ax_ns);
                    perm_sc.setTitle_(sc_ns);
                }
            }
        }
    }

    fn http_get(path: &str) {
        let base = unsafe { HANDLER_SINGLETON.as_ref().unwrap().lock().unwrap().api_base.clone() };
        let url = format!("{}{}", base, path);
        thread::spawn(move || { let _ = reqwest::blocking::get(&url); });
    }

    fn ensure_agent_running() {
        // quick healthz
        let base = std::env::var("PANEL_ADDR").unwrap_or_else(|_| "127.0.0.1:49219".to_string());
        let url = format!("http://{}/healthz", base);
        let ok = reqwest::blocking::get(&url).map(|r| r.status().is_success()).unwrap_or(false);
        if ok { return; }
        // Try to spawn agent-daemon from bundle: Contents/Resources/bin/agent-daemon
        if let Ok(mut exe) = std::env::current_exe() {
            // exe: /path/Ripor.app/Contents/MacOS/RiporUI
            // resources: /path/Ripor.app/Contents/Resources/bin/agent-daemon
            for _ in 0..2 { exe.pop(); } // remove MacOS/RiporUI
            let agent = exe.join("Resources").join("bin").join("agent-daemon");
            if agent.exists() {
                let _ = std::process::Command::new(agent)
                    .env("RUST_LOG", std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
                    .spawn();
            }
        }
    }

    fn bundle_icon_path() -> Option<String> {
        if let Ok(mut exe) = std::env::current_exe() {
            exe.pop(); exe.pop(); // Contents/MacOS
            let p = exe.join("Resources").join("iconTemplate.png");
            if p.exists() { return Some(p.display().to_string()); }
        }
        None
    }

    fn bundle_agent_path() -> Option<String> {
        if let Ok(mut exe) = std::env::current_exe() {
            // /path/Ripor.app/Contents/MacOS/RiporUI -> /path/Ripor.app/Contents
            exe.pop(); // MacOS
            exe.pop(); // Contents
            let p = exe.join("Resources").join("bin").join("agent-daemon");
            if p.exists() { return Some(p.display().to_string()); }
        }
        None
    }

    // Toggle/Login Item via SMAppService (macOS 13+) if available
    extern "C" {
        fn ripor_loginitem_register(bundle_id: *const std::os::raw::c_char) -> bool;
        fn ripor_loginitem_unregister(bundle_id: *const std::os::raw::c_char) -> bool;
        fn ripor_loginitem_is_registered(bundle_id: *const std::os::raw::c_char) -> bool;
    }
    unsafe fn sm_loginitem_toggle(bundle_id: &str, enable: bool) -> bool {
        use std::ffi::CString;
        let c = CString::new(bundle_id).unwrap();
        if enable { ripor_loginitem_register(c.as_ptr()) } else { ripor_loginitem_unregister(c.as_ptr()) }
    }

    // Fallback: LaunchAgent in ~/Library/LaunchAgents
    fn launchagent_meta() -> (String, String) {
        let label = "com.ripor.agent".to_string();
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let plist_path = format!("{}/Library/LaunchAgents/{}.plist", home, label);
        (label, plist_path)
    }

    fn enable_login_item() -> std::io::Result<()> {
        let (label, plist_path) = launchagent_meta();
        if let Some(agent_path) = bundle_agent_path() {
            let plist = format!(r#"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\"><dict>
  <key>Label</key><string>{label}</string>
  <key>RunAtLoad</key><true/>
  <key>ProgramArguments</key>
  <array>
    <string>{agent}</string>
  </array>
</dict></plist>
"#, label=label, agent=agent_path);
            if let Some(dir) = std::path::Path::new(&plist_path).parent() { std::fs::create_dir_all(dir)?; }
            std::fs::write(&plist_path, plist.as_bytes())?;
            let uid = String::from_utf8(std::process::Command::new("/usr/bin/id").arg("-u").output().ok().map(|o| o.stdout).unwrap_or_default()).unwrap_or_default().trim().to_string();
            let domain = format!("gui/{}", uid);
            let _ = std::process::Command::new("/bin/launchctl").args(["bootout", &domain, &label]).output();
            let _ = std::process::Command::new("/bin/launchctl").args(["bootstrap", &domain, &plist_path]).output();
        }
        Ok(())
    }

    fn disable_login_item() -> std::io::Result<()> {
        let (label, plist_path) = launchagent_meta();
        let uid = String::from_utf8(std::process::Command::new("/usr/bin/id").arg("-u").output().ok().map(|o| o.stdout).unwrap_or_default()).unwrap_or_default().trim().to_string();
        let domain = format!("gui/{}", uid);
        let _ = std::process::Command::new("/bin/launchctl").args(["bootout", &domain, &label]).output();
        let _ = std::fs::remove_file(&plist_path);
        Ok(())
    }

    fn is_login_enabled() -> bool {
        let (_, plist_path) = launchagent_meta();
        std::path::Path::new(&plist_path).exists()
    }

    extern "C" fn onToggleLogin(_this: &Object, _cmd: Sel, _sender: id) {
        // Read current state via SMAppService if available; fallback LaunchAgent
        let curr_enabled = unsafe {
            use std::ffi::CString;
            let c = CString::new("com.ripor.Ripor.LoginItem").unwrap();
            if ripor_loginitem_is_registered(c.as_ptr()) { true } else { is_login_enabled() }
        };
        let enable = !curr_enabled;
        let ok = unsafe { sm_loginitem_toggle("com.ripor.Ripor.LoginItem", enable) };
        if !ok { let _ = if enable { enable_login_item() } else { disable_login_item() }; }
    }

    extern "C" fn onOpenPanel(this: &Object, _cmd: Sel, _sender: id) {
        unsafe {
            let url_s = &HANDLER_SINGLETON.as_ref().unwrap().lock().unwrap().panel_url.clone();
            let ns_url = NSURL::alloc(nil).initWithString_(NSString::alloc(nil).init_str(url_s));
            let ws: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: () = msg_send![ws, openURL: ns_url];
        }
    }
    extern "C" fn onOpenAx(_this: &Object, _cmd: Sel, _sender: id) { http_get("/permissions/open/accessibility"); }
    extern "C" fn onOpenSc(_this: &Object, _cmd: Sel, _sender: id) { http_get("/permissions/open/screen"); }
    extern "C" fn onPause15(_this: &Object, _cmd: Sel, _sender: id) { http_get("/pause?minutes=15"); }
    extern "C" fn onPause60(_this: &Object, _cmd: Sel, _sender: id) { http_get("/pause?minutes=60"); }
    extern "C" fn onResume(_this: &Object, _cmd: Sel, _sender: id) { http_get("/pause/clear"); }
    extern "C" fn onQuit(this: &Object, _cmd: Sel, _sender: id) {
        unsafe {
            let app = NSApp();
            let _: () = msg_send![app, terminate: this];
        }
    }

    fn register_handler_class() -> *const Class {
        let mut decl = ClassDecl::new("RiporStatusHandler", class!(NSObject)).unwrap();
        unsafe {
            decl.add_method(sel!(onOpenPanel:), onOpenPanel as extern "C" fn(&Object, Sel, id));
            decl.add_method(sel!(onOpenAx:), onOpenAx as extern "C" fn(&Object, Sel, id));
            decl.add_method(sel!(onOpenSc:), onOpenSc as extern "C" fn(&Object, Sel, id));
            decl.add_method(sel!(onPause15:), onPause15 as extern "C" fn(&Object, Sel, id));
            decl.add_method(sel!(onPause60:), onPause60 as extern "C" fn(&Object, Sel, id));
            decl.add_method(sel!(onResume:), onResume as extern "C" fn(&Object, Sel, id));
            decl.add_method(sel!(onQuit:), onQuit as extern "C" fn(&Object, Sel, id));
            decl.add_method(sel!(onToggleLogin:), onToggleLogin as extern "C" fn(&Object, Sel, id));
            // no-op to satisfy targets of read-only items
            extern "C" fn noop(_this: &Object, _cmd: Sel, _sender: id) {}
            decl.add_method(sel!(noop:), noop as extern "C" fn(&Object, Sel, id));
        }
        decl.register()
    }
}

#[cfg(not(target_os = "macos"))]
mod app {
    pub fn run() {
        eprintln!("agent-ui-macos solo está soportado en macOS");
    }
}

fn main() {
    app::run();
}
