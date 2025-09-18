#![cfg(target_os = "macos")]
#![allow(non_snake_case)]

use cocoa::appkit::{NSApp, NSApplication};
use cocoa::base::{id, nil};
use cocoa::foundation::NSAutoreleasePool;

fn main() {
    unsafe {
        let _pool = NSAutoreleasePool::new(nil);
        // Attempt to run bundled agent-daemon: ../Resources/bin/agent-daemon
        if let Ok(mut exe) = std::env::current_exe() {
            // exe = Ripor.app/Contents/Library/LoginItems/RiporHelper.app/Contents/MacOS/agent-login-macos
            // go up to Ripor.app/Contents
            for _ in 0..4 { exe.pop(); }
            let agent = exe.join("Resources").join("bin").join("agent-daemon");
            if agent.exists() {
                let _ = std::process::Command::new(agent)
                    .env("RUST_LOG", std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
                    .spawn();
            }
        }
        // Exit quickly; Login Item can be keep-alive by system if needed
        let app = NSApp();
        let _: () = msg_send::msg_send![app, terminate: nil];
    }
}

