//! `quin install`: put the running binary on the user's PATH and, on Linux,
//! register a desktop entry and icon so launchers find it. Everything goes
//! under the user's home; nothing needs root.

use std::path::{Path, PathBuf};

/// A 256px rendering of `assets/icon/icon.png` (the full-size original stays
/// out of the binary).
const ICON_PNG: &[u8] = include_bytes!("../assets/icon/icon-256.png");

const DESKTOP_ENTRY: &str = "\
[Desktop Entry]
Type=Application
Name=quin
GenericName=Text Editor
Comment=A small, fast, keyboard-driven code editor
Exec=quin %F
Icon=quin
Terminal=false
Categories=Development;TextEditor;Utility;
MimeType=text/plain;text/x-rust;text/x-python;text/x-csrc;text/x-c++src;text/x-shellscript;text/markdown;application/json;application/toml;inode/directory;
Keywords=editor;code;text;
StartupWMClass=quin
";

/// What `install` wrote, for the message at the end.
pub struct Installed {
    pub binary: PathBuf,
    pub desktop: Option<PathBuf>,
    pub icon: Option<PathBuf>,
    pub on_path: bool,
    /// Something was there before: an update rather than a first install.
    pub updated: bool,
}

/// Write a file, replacing whatever was there (removed first so caches keyed
/// on inode or mtime notice the change).
fn replace_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

/// Install into `home` (`~/.local/bin`, `~/.local/share/...`).
pub fn install(home: &Path, exe: &Path) -> Result<Installed, String> {
    let bin_dir = home.join(".local/bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| format!("{}: {e}", bin_dir.display()))?;
    let binary = bin_dir.join(if cfg!(windows) { "quin.exe" } else { "quin" });
    let updated = binary.exists();
    // Copy over a temporary name, then rename over the old binary: a running
    // `quin` keeps its (now unlinked) file and the new one replaces it at once.
    let staged = bin_dir.join("quin.installing");
    std::fs::copy(exe, &staged).map_err(|e| format!("copy to {}: {e}", staged.display()))?;
    std::fs::rename(&staged, &binary).map_err(|e| format!("{}: {e}", binary.display()))?;

    let (mut desktop, mut icon) = (None, None);
    if cfg!(target_os = "linux") {
        let apps = home.join(".local/share/applications");
        let hicolor = home.join(".local/share/icons/hicolor");
        let sized = hicolor.join("256x256/apps");
        // An earlier version installed an SVG; it must not shadow the new icon.
        let _ = std::fs::remove_file(hicolor.join("scalable/apps/quin.svg"));
        for dir in [&apps, &sized] {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let desktop_path = apps.join("quin.desktop");
        replace_file(&desktop_path, DESKTOP_ENTRY.as_bytes())?;
        let icon_path = sized.join("quin.png");
        replace_file(&icon_path, ICON_PNG)?;
        // Icon caches are keyed on the theme directory's mtime: bump it, then
        // nudge whichever cache tools exist (all optional).
        let now = std::time::SystemTime::now();
        for dir in [&hicolor, &sized] {
            let _ = std::fs::File::open(dir).and_then(|f| f.set_times(std::fs::FileTimes::new().set_modified(now)));
        }
        for (tool, arg) in [
            ("update-desktop-database", apps.clone()),
            ("gtk-update-icon-cache", hicolor.clone()),
            ("xdg-desktop-menu", PathBuf::from("forceupdate")),
        ] {
            let _ = std::process::Command::new(tool).arg(arg).output();
        }
        for tool in ["kbuildsycoca6", "kbuildsycoca5"] {
            let _ = std::process::Command::new(tool).output();
        }
        desktop = Some(desktop_path);
        icon = Some(icon_path);
    }
    let on_path = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == bin_dir));
    Ok(Installed { binary, desktop, icon, on_path, updated })
}

/// The command line entry point: `quin install`.
pub fn run_cli() -> Result<(), String> {
    let home =
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).ok_or("cannot find the home directory")?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this binary: {e}"))?;
    let done = install(&home, &exe)?;
    println!("{} {}", if done.updated { "updated" } else { "installed" }, done.binary.display());
    if let Some(desktop) = &done.desktop {
        println!("desktop entry {}", desktop.display());
    }
    if let Some(icon) = &done.icon {
        println!("icon {}", icon.display());
    }
    if done.updated && done.desktop.is_some() {
        println!("note: some launchers show the old icon until you log out and in again");
    }
    if !done.on_path {
        println!("note: {} is not on your PATH; add it to your shell profile", done.binary.parent().unwrap_or(&done.binary).display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_binary_entry_and_icon_under_home() {
        let home = std::env::temp_dir().join(format!("quin-install-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let exe = std::env::current_exe().unwrap();
        let done = install(&home, &exe).unwrap();
        assert!(done.binary.is_file() && !done.updated);
        assert!(install(&home, &exe).unwrap().updated, "a second install overwrites");
        assert_eq!(std::fs::metadata(&done.binary).unwrap().len(), std::fs::metadata(&exe).unwrap().len());
        if cfg!(target_os = "linux") {
            let entry = std::fs::read_to_string(done.desktop.unwrap()).unwrap();
            assert!(entry.contains("Exec=quin %F") && entry.contains("Icon=quin"));
            assert!(std::fs::read(done.icon.unwrap()).unwrap().starts_with(b"\x89PNG"));
        }
        std::fs::remove_dir_all(&home).unwrap();
    }
}
