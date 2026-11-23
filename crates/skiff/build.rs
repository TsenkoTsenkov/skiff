//! Build script of the app.
//!
//! * Tells the code which target it is built for (`SKIFF_TARGET`, printed
//!   by `skiff --version`).
//! * On Windows targets, embeds the icon and the version information that
//!   Explorer, the taskbar and the "Details" tab of the file properties
//!   show. Does nothing else on other targets.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icons/skiff.ico");
    println!("cargo:rerun-if-env-changed=SKIFF_BUILD");
    println!("cargo:rerun-if-env-changed=WINDRES");

    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=SKIFF_TARGET={target}");

    // (The target's OS, not the host's: this also runs when cross-compiling.)
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The build number of a release (see update.rs); 0 for a local build.
    let build: u64 = env::var("SKIFF_BUILD").ok().and_then(|b| b.trim().parse().ok()).unwrap_or(0);
    if let Err(e) = embed_windows_resources(build) {
        // A release must not ship without its icon; a local build (no
        // resource compiler installed, say) is still worth having.
        if build != 0 {
            panic!("could not embed the Windows icon and version info: {e}");
        }
        println!("cargo:warning=skiff.exe is built without icon and version info: {e}");
    }
}

fn embed_windows_resources(build: u64) -> std::io::Result<()> {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icons/skiff.ico");
    // What Windows shows as the program's name (Task Manager, "Open with",
    // the taskbar's context menu) is the file description.
    res.set("FileDescription", "Skiff");
    res.set("ProductName", "Skiff");
    res.set("InternalName", "skiff");
    res.set("OriginalFilename", "skiff.exe");
    if let Ok(description) = env::var("CARGO_PKG_DESCRIPTION") {
        res.set("Comments", &description);
    }
    res.set("LegalCopyright", "MIT License");
    // The numeric version is major.minor.patch.build, 16 bits each
    // (winresource has filled in the first three). The build number only
    // goes into the file version; a count above 65535 is cut off rather
    // than spilling into the patch field.
    let (major, minor, patch) = (pkg_number("MAJOR"), pkg_number("MINOR"), pkg_number("PATCH"));
    let version = major << 48 | minor << 32 | patch << 16;
    res.set_version_info(winresource::VersionInfo::FILEVERSION, version | build.min(0xFFFF));
    if build != 0 {
        res.set("FileVersion", &format!("{major}.{minor}.{patch}.{build}"));
    }
    // Only into skiff.exe, not into test binaries.
    res.compile_for("skiff")
}

fn pkg_number(part: &str) -> u64 {
    env::var(format!("CARGO_PKG_VERSION_{part}")).ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0).min(0xFFFF)
}
