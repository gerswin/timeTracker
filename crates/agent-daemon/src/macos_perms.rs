#[cfg(target_os = "macos")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct PermsStatus {
    pub accessibility_ok: bool,
    pub screen_recording_ok: bool,
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool; // CFDictionaryRef
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[cfg(target_os = "macos")]
pub fn check_permissions() -> PermsStatus {
    let accessibility_ok = unsafe { AXIsProcessTrusted() };
    let screen_recording_ok = unsafe { CGPreflightScreenCaptureAccess() };
    PermsStatus {
        accessibility_ok,
        screen_recording_ok,
    }
}

#[cfg(target_os = "macos")]
pub fn prompt_permissions() -> PermsStatus {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFMutableDictionary;
    use core_foundation::string::CFString;
    use std::process::Command;
    unsafe {
        // Prompt Accessibility via AXIsProcessTrustedWithOptions
        let key = CFString::new("kAXTrustedCheckOptionPrompt");
        let mut dict = CFMutableDictionary::<CFString, CFBoolean>::new();
        dict.set(key.clone(), CFBoolean::true_value());
        let _ =
            AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef() as *const std::ffi::c_void);
    }
    // Solicitar Screen Recording con API pública (Catalina+). Puede que muestre el diálogo una sola vez.
    unsafe {
        let _ = CGRequestScreenCaptureAccess();
    }
    // Además, abrimos System Settings en la sección adecuada para que el usuario verifique el toggle.
    let _ = Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn();
    check_permissions()
}

#[cfg(target_os = "macos")]
pub fn screen_recording_allowed() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

#[cfg(target_os = "macos")]
pub fn open_accessibility_pane() {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

#[cfg(target_os = "macos")]
pub fn open_screencapture_pane() {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn();
}
