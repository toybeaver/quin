//! System clipboard through the platform's command line tools (like Helix does),
//! with an in-process fallback when none is available. This keeps the editor
//! free of heavy clipboard libraries. Besides text, the clipboard can hold an
//! image or a copied file, which Markdown buffers paste as an image tag.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Something other than text on the clipboard.
#[derive(Debug, Clone, PartialEq)]
pub enum Media {
    /// Image data (a screenshot, an image copied in a browser).
    Bytes(Vec<u8>),
    /// A file copied in a file manager.
    File(PathBuf),
}

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
    /// What `media` gives when no system clipboard tool is available (tests).
    pub internal_media: Option<Media>,
}

impl Clipboard {
    pub fn new() -> Clipboard {
        Clipboard { backend: None, internal: String::new(), internal_media: None }
    }

    /// A clipboard that never touches the system one (tests run in parallel).
    #[cfg(test)]
    pub fn in_memory() -> Clipboard {
        Clipboard { backend: Some(None), internal: String::new(), internal_media: None }
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
        self.internal_media = None;
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

    /// An image or a copied file on the clipboard, if there is one. Each
    /// platform tool is asked which types it holds first, so plain text
    /// costs one quick call.
    pub fn media(&mut self) -> Option<Media> {
        let Some(backend) = self.backend() else { return self.internal_media.clone() };
        match backend.copy[0] {
            "wl-copy" => {
                let types = run(&["wl-paste", "--list-types"])?;
                media_from_types(&String::from_utf8_lossy(&types), |kind| run(&["wl-paste", "-n", "-t", kind]))
            }
            "xclip" => {
                let types = run(&["xclip", "-selection", "clipboard", "-t", "TARGETS", "-o"])?;
                media_from_types(&String::from_utf8_lossy(&types), |kind| run(&["xclip", "-selection", "clipboard", "-t", kind, "-o"]))
            }
            "pbcopy" => {
                let info = String::from_utf8_lossy(&run(&["osascript", "-e", "clipboard info"])?).into_owned();
                for class in ["PNGf", "JPEG"] {
                    if info.contains(&format!("«class {class}»")) {
                        let script = format!("the clipboard as «class {class}»");
                        return Some(Media::Bytes(applescript_data(&String::from_utf8_lossy(&run(&["osascript", "-e", &script])?))?));
                    }
                }
                if info.contains("«class furl»") {
                    let path = run(&["osascript", "-e", "POSIX path of (the clipboard as «class furl»)"])?;
                    return Some(Media::File(PathBuf::from(String::from_utf8_lossy(&path).trim())));
                }
                None
            }
            "clip" => {
                let script = "Add-Type -AssemblyName System.Windows.Forms, System.Drawing; \
                    if ([Windows.Forms.Clipboard]::ContainsImage()) { $m = New-Object IO.MemoryStream; \
                    [Windows.Forms.Clipboard]::GetImage().Save($m, [Drawing.Imaging.ImageFormat]::Png); \
                    'png:' + [Convert]::ToBase64String($m.ToArray()) } \
                    elseif ([Windows.Forms.Clipboard]::ContainsFileDropList()) { 'file:' + [Windows.Forms.Clipboard]::GetFileDropList()[0] }";
                let out = run(&["powershell", "-NoProfile", "-STA", "-Command", script])?;
                let out = String::from_utf8_lossy(&out);
                let out = out.trim();
                if let Some(data) = out.strip_prefix("png:") {
                    return Some(Media::Bytes(base64_decode(data)?));
                }
                out.strip_prefix("file:").map(|path| Media::File(PathBuf::from(path)))
            }
            _ => None, // xsel only handles text
        }
    }
}

/// Run a clipboard tool and take its output, if it succeeded.
fn run(command: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new(command[0]).args(&command[1..]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    (output.status.success() && !output.stdout.is_empty()).then_some(output.stdout)
}

/// Pick the media out of a list of MIME types (one per line, as `wl-paste`
/// and `xclip` give them): an image first, then a copied file.
fn media_from_types(types: &str, read: impl Fn(&str) -> Option<Vec<u8>>) -> Option<Media> {
    let types: Vec<&str> = types.lines().map(str::trim).collect();
    for kind in ["image/png", "image/jpeg"] {
        if types.contains(&kind) {
            return read(kind).map(Media::Bytes);
        }
    }
    if types.contains(&"text/uri-list") {
        let list = read("text/uri-list")?;
        return file_from_uri_list(&String::from_utf8_lossy(&list)).map(Media::File);
    }
    None
}

/// The first local file in a `text/uri-list` (`file:///home/me/a%20b.png`).
pub fn file_from_uri_list(list: &str) -> Option<PathBuf> {
    let uri = list.lines().map(str::trim).find(|l| l.starts_with("file://"))?;
    let path = uri.strip_prefix("file://")?;
    // `file://host/path`: only the local host (empty or `localhost`) is ours.
    let path = path.strip_prefix("localhost").unwrap_or(path);
    if !path.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(percent_decode(path)))
}

/// `%20` and friends back to bytes.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The bytes of an AppleScript data literal: `«data PNGf89504E47...»`.
fn applescript_data(text: &str) -> Option<Vec<u8>> {
    let start = text.find("«data ")? + "«data ".len() + 4; // skip the four-letter class
    let hex = text.get(start..)?.split('»').next()?;
    (0..hex.len() / 2).map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()).collect()
}

/// Standard base64 (with `=` padding) to bytes.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let chars: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::with_capacity(chars.len() * 3 / 4);
    for chunk in chars.chunks(4) {
        let mut bits = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            bits |= value(c)? << (18 - 6 * i);
        }
        let bytes = bits.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

fn tool_exists(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(name);
        candidate.is_file() || dir.join(format!("{name}.exe")).is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_copied_files_and_image_data() {
        let list = "# comment\r\nfile:///home/me/my%20pic.png\r\nfile:///other.png\r\n";
        assert_eq!(file_from_uri_list(list), Some(PathBuf::from("/home/me/my pic.png")));
        assert_eq!(file_from_uri_list("https://x/y.png"), None);
        assert_eq!(applescript_data("«data PNGf89504E47»"), Some(vec![0x89, 0x50, 0x4e, 0x47]));
        assert_eq!(base64_decode("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64_decode("aGk="), Some(b"hi".to_vec()));
        let picked = media_from_types("text/html\nimage/png\n", |kind| Some(kind.as_bytes().to_vec()));
        assert_eq!(picked, Some(Media::Bytes(b"image/png".to_vec())), "an image wins over the text types");
        assert_eq!(media_from_types("text/plain\nUTF8_STRING\n", |_| None), None);
    }
}
