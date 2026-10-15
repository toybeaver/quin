//! System clipboard through the platform's command line tools (like Helix does),
//! with an in-process fallback when none is available. This keeps the editor
//! free of heavy clipboard libraries.

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy)]
struct Backend {
    copy: &'static [&'static str],
    paste: &'static [&'static str],
}

const BACKENDS: &[(&str, Backend)] = &[
    ("wl-copy", Backend { copy: &["wl-copy"], paste: &["wl-paste", "-n"] }),
    ("xclip", Backend { copy: &["xclip", "-selection", "clipboard", "-i"], paste: &["xclip", "-selection", "clipboard", "-o"] }),
    ("xsel", Backend { copy: &["xsel", "--clipboard", "-i"], paste: &["xsel", "--clipboard", "-o"] }),
    ("pbcopy", Backend { copy: &["pbcopy"], paste: &["pbpaste"] }),
    ("clip", Backend { copy: &["clip"], paste: &["powershell", "-NoProfile", "-Command", "Get-Clipboard"] }),
];

pub struct Clipboard {
    backend: Option<Option<Backend>>,
    /// Last copied text, used when no system clipboard tool is available.
    internal: String,
}

impl Clipboard {
    pub fn new() -> Clipboard {
        Clipboard { backend: None, internal: String::new() }
    }

    /// A clipboard that never touches the system one (tests run in parallel).
    #[cfg(test)]
    pub fn in_memory() -> Clipboard {
        Clipboard { backend: Some(None), internal: String::new() }
    }

    /// Find a usable tool the first time the clipboard is used.
    fn backend(&mut self) -> Option<Backend> {
        if let Some(found) = self.backend {
            return found;
        }
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let found = BACKENDS
            .iter()
            .filter(|(name, _)| !(*name == "wl-copy" && !wayland))
            .find(|(name, _)| tool_exists(name))
            .map(|(_, backend)| *backend);
        self.backend = Some(found);
        found
    }

    pub fn copy(&mut self, text: &str) -> Result<(), String> {
        self.internal = text.to_string();
        let Some(backend) = self.backend() else { return Ok(()) };
        let mut child = Command::new(backend.copy[0])
            .args(&backend.copy[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{}: {e}", backend.copy[0]))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        // wl-copy forks and keeps running; don't wait for it longer than needed.
        let _ = child.wait();
        Ok(())
    }

    pub fn paste(&mut self) -> Result<String, String> {
        let Some(backend) = self.backend() else { return Ok(self.internal.clone()) };
        let output = Command::new(backend.paste[0])
            .args(&backend.paste[1..])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("{}: {e}", backend.paste[0]))?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        if backend.paste[0] == "powershell" {
            text = text.replace("\r\n", "\n");
            if text.ends_with('\n') {
                text.pop();
            }
        }
        Ok(text)
    }
}

fn tool_exists(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(name);
        candidate.is_file() || dir.join(format!("{name}.exe")).is_file()
    })
}
