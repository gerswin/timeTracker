fn main() {
    #[cfg(target_os = "macos")]
    {
        let mut build = cc::Build::new();
        build.file("src/macos_loginitem.m");
        build.flag("-fobjc-arc");
        build.compile("macos_loginitem");
        // Link ServiceManagement framework
        println!("cargo:rustc-link-lib=framework=ServiceManagement");
        println!("cargo:rustc-link-lib=framework=AppKit");
    }
}

