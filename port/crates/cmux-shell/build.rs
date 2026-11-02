//! Build-time staging of the bundled ConPTY pair.
//!
//! The repo carries Microsoft's redistributable ConPTY (from the
//! `Microsoft.Windows.Console.ConPTY` NuGet package, MIT; provenance in
//! `third_party/conpty/ORIGIN.md`) under `third_party/conpty/`. At build
//! time the `conpty.dll`/`OpenConsole.exe` x64 pair is copied next to
//! `cmux-shell.exe` in the cargo profile directory, so `portable-pty`'s
//! sideload path (`ConPtyFuncs::open("conpty.dll")` — plain `LoadLibraryW`,
//! application directory first) finds it and panes run on the newer host.
//! That host is required for terminal mouse reporting: the stock kernel32
//! ConPTY swallows the child's mouse-mode requests on the output side
//! (ADR-0017).
//!
//! Degradation: if the pair is absent the build still succeeds and panes
//! fall back to the in-box ConPTY (selection-only mouse), the pre-bundle
//! behavior.

use std::env;
use std::path::{Path, PathBuf};

/// Names of the sideload pair (see third_party/conpty/ORIGIN.md).
const DLL_NAME: &str = "conpty.dll";
const CONSOLE_NAME: &str = "OpenConsole.exe";

fn main() {
    if !cfg!(target_os = "windows") {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let third = manifest
        .join("..")
        .join("..")
        .join("third_party")
        .join("conpty");

    // OUT_DIR looks like <target>/<profile>/build/cmux-shell-<hash>/out;
    // the profile directory is the one directly inside <target>.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let profile = profile_dir(&out_dir);
    let Some(profile) = profile else {
        eprintln!(
            "cargo:warning=cmux-shell: could not locate the cargo profile directory; \
             the bundled ConPTY pair was not staged (panes use the in-box ConPTY)"
        );
        return;
    };

    let dll = third.join(DLL_NAME);
    let console = third.join(CONSOLE_NAME);
    if !(dll.is_file() && console.is_file()) {
        eprintln!(
            "cargo:warning=cmux-shell: bundled ConPTY pair missing at {}; \
             panes use the in-box ConPTY",
            third.display()
        );
        return;
    }
    if let Err(error) = copy_pair(&dll, &console, &profile) {
        eprintln!("cargo:warning=cmux-shell: failed to stage the bundled ConPTY pair: {error}");
        return;
    }

    // Re-run whenever the pair or this script changes so updates propagate.
    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        third
            .join(DLL_NAME)
            .display()
            .to_string()
            .replace('\\', "/")
    );
    println!(
        "cargo:rerun-if-changed={}",
        third
            .join(CONSOLE_NAME)
            .display()
            .to_string()
            .replace('\\', "/")
    );
}

/// The profile directory: walk OUT_DIR up to the directory whose parent is
/// named `target`, and take that directory ("<target>/<profile>").
fn profile_dir(out_dir: &Path) -> Option<PathBuf> {
    let mut ancestors = out_dir.ancestors().peekable();
    while let Some(step) = ancestors.next() {
        if step.file_name().is_some_and(|n| n == "build") {
            // step = .../target/<profile>/build → next is <target>/<profile>
            let profile = ancestors.next()?;
            if profile
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n == "target")
            {
                return Some(profile.to_path_buf());
            }
        }
    }
    None
}

fn copy_pair(
    dll: &std::path::Path,
    console: &std::path::Path,
    profile: &std::path::Path,
) -> std::io::Result<()> {
    let dest_dll = profile.join(DLL_NAME);
    let dest_console = profile.join(CONSOLE_NAME);
    let needs = |src: &std::path::Path, dst: &std::path::Path| {
        std::fs::metadata(dst)
            .and_then(|d| d.modified())
            .and_then(|m| src.metadata().and_then(|s| s.modified()).map(|sm| sm > m))
            .map(|src_newer| src_newer || !dst.exists())
            .unwrap_or(true)
    };
    if needs(dll, &dest_dll) {
        std::fs::copy(dll, &dest_dll)?;
    }
    if needs(console, &dest_console) {
        std::fs::copy(console, &dest_console)?;
    }
    eprintln!("cmux-shell build: staged bundled ConPTY pair next to the shell executable");
    Ok(())
}
