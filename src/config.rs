//! User configuration: `config.toml` in the OS configuration directory.
//!
//! - Linux/BSD: `$XDG_CONFIG_HOME/quin/config.toml`, i.e. `~/.config/quin/config.toml`
//! - macOS: `~/Library/Application Support/quin/config.toml`
//! - Windows: `%APPDATA%\quin\config.toml`
//! - any OS: `$QUIN_CONFIG_DIR/config.toml` when that variable is set
//!
//! The file is flat TOML, one `key = value` per line:
//!
//! ```toml
//! font-size    = 14
//! font-family  = "JetBrainsMono Nerd Font"  # an installed font; default: the embedded Iosevka
//! font         = "/path/to/mono.ttf"   # a font file instead of a family (bold-font too)
//! ligatures    = true
//! theme        = "dark"                # dark, light, any bundled scheme, or a file in themes/
//! tab-width    = 4
//! line-numbers = "on"                  # "on", "off", "relative"
//! cursorline   = true
//! scrolloff    = 3
//! auto-pairs   = true
//! wrap         = false                 # soft-wrap long lines (`:wrap` toggles it for the session)
//! scroll-animation = 120               # milliseconds for scroll jumps to slide; 0 for none
//! cursor-animation = 80                # milliseconds for the cursor to slide; 0 for none
//! shell        = "/bin/zsh"
//! ```
//! Next to it, `syntax/*.syn` and `themes/*.theme` add or replace languages and themes.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub font_size: f32,
    pub font_family: Option<String>,
    pub font: Option<PathBuf>,
    pub bold_font: Option<PathBuf>,
    pub ligatures: bool,
    pub theme: String,
    pub tab_width: usize,
    pub line_numbers: bool,
    pub relative_line_numbers: bool,
    pub cursorline: bool,
    pub scrolloff: usize,
    pub auto_pairs: bool,
    /// Soft-wrap long lines instead of scrolling sideways.
    pub wrap: bool,
    /// Milliseconds a scroll jump takes to slide; 0 jumps at once.
    pub scroll_animation: u64,
    /// Milliseconds the cursor takes to slide to its new place; 0 jumps at once.
    pub cursor_animation: u64,
    pub shell: Option<String>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            font_size: 16.0,
            font_family: None,
            font: None,
            bold_font: None,
            ligatures: true,
            theme: "bright-lights".to_string(),
            tab_width: 4,
            line_numbers: true,
            relative_line_numbers: false,
            cursorline: true,
            scrolloff: 3,
            auto_pairs: true,
            wrap: false,
            scroll_animation: 120,
            cursor_animation: 80,
            shell: None,
        }
    }
}

/// The configuration directory for this OS (see the module comment).
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QUIN_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support/quin"));
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join("quin"));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir).join("quin"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("quin"))
}

pub fn config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

impl Config {
    /// Load the config file, printing problems to stderr. Missing file = defaults.
    pub fn load() -> Config {
        let mut config = Config::default();
        let Some(path) = config_file() else { return config };
        let Ok(source) = std::fs::read_to_string(&path) else { return config };
        for (i, line) in source.lines().enumerate() {
            if let Err(err) = config.apply_line(line) {
                eprintln!("quin: {}:{}: {err}", path.display(), i + 1);
            }
        }
        config
    }

    /// Parse one `key = value` line (comments and blank lines are fine).
    pub fn apply_line(&mut self, line: &str) -> Result<(), String> {
        let line = strip_comment(line).trim();
        if line.is_empty() || line.starts_with('[') {
            return Ok(());
        }
        let (key, value) = line.split_once('=').ok_or_else(|| format!("expected `key = value`, got `{line}`"))?;
        self.set(key.trim(), unquote(value.trim()))
    }

    /// Change one setting.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let bool_value = || match value {
            "on" | "true" | "yes" | "1" | "" => Ok(true),
            "off" | "false" | "no" | "0" => Ok(false),
            _ => Err(format!("expected true/false for {key}, got `{value}`")),
        };
        match key {
            "font-size" => self.font_size = value.parse::<f32>().map_err(|_| format!("bad font-size `{value}`"))?.clamp(5.0, 72.0),
            "font-family" => self.font_family = Some(value.to_string()).filter(|s| !s.is_empty()),
            "font" => self.font = Some(expand_home(value)),
            "bold-font" => self.bold_font = Some(expand_home(value)),
            "ligatures" => self.ligatures = bool_value()?,
            "theme" => self.theme = value.to_string(),
            "tab-width" => self.tab_width = value.parse::<usize>().map_err(|_| format!("bad tab-width `{value}`"))?.clamp(1, 16),
            "line-numbers" => match value {
                "relative" => {
                    self.line_numbers = true;
                    self.relative_line_numbers = true;
                }
                _ => {
                    self.line_numbers = bool_value()?;
                    self.relative_line_numbers = false;
                }
            },
            "cursorline" => self.cursorline = bool_value()?,
            "scrolloff" => self.scrolloff = value.parse().map_err(|_| format!("bad scrolloff `{value}`"))?,
            "auto-pairs" => self.auto_pairs = bool_value()?,
            "wrap" => self.wrap = bool_value()?,
            "scroll-animation" => self.scroll_animation = value.parse().map_err(|_| format!("bad scroll-animation `{value}`"))?,
            "cursor-animation" => self.cursor_animation = value.parse().map_err(|_| format!("bad cursor-animation `{value}`"))?,
            "shell" => self.shell = Some(value.to_string()),
            _ => return Err(format!("unknown setting `{key}`")),
        }
        Ok(())
    }

    /// A fresh config file, with the current values of the main settings.
    pub fn template(&self, font_family: &str) -> String {
        format!(
            "# quin configuration (flat TOML). Edit, save, restart.\n\
             font-size = {}\n\
             # font-family = \"{}\"   # an installed font; the default is the embedded Iosevka\n\
             theme = \"{}\"   # :theme shows every bundled theme\n\
             \n\
             # ligatures = true\n\
             # tab-width = 4\n\
             # line-numbers = \"on\"   # \"on\", \"off\", \"relative\"\n\
             # cursorline = true\n\
             # scrolloff = 3\n\
             # auto-pairs = true\n\
             # wrap = false   # soft-wrap long lines; :wrap toggles it\n\
             # scroll-animation = 120   # milliseconds; 0 for none\n\
             # cursor-animation = 80    # milliseconds; 0 for none\n\
             # shell = \"/bin/zsh\"\n",
            self.font_size, font_family, self.theme
        )
    }
}

/// Drop a `#` comment, unless the `#` sits inside quotes.
fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => return &line[..i],
            _ => {}
        }
    }
    line
}

fn unquote(value: &str) -> &str {
    value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value)
}

fn expand_home(path: &str) -> PathBuf {
    crate::files::expand_home(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_toml_lines() {
        let mut config = Config::default();
        config.apply_line("font-size = 16   # bigger").unwrap();
        config.apply_line("font-family = \"Iosevka Nerd Font\"").unwrap();
        config.apply_line("theme = \"catppuccin-mocha\"").unwrap();
        config.apply_line("line-numbers = \"relative\"").unwrap();
        config.apply_line("ligatures = false").unwrap();
        config.apply_line("").unwrap();
        config.apply_line("# comment").unwrap();
        assert_eq!(config.font_size, 16.0);
        assert_eq!(config.font_family.as_deref(), Some("Iosevka Nerd Font"));
        assert_eq!(config.theme, "catppuccin-mocha");
        assert!(config.relative_line_numbers && !config.ligatures);
        assert!(config.apply_line("nonsense").is_err());
        assert!(config.apply_line("colour = 1").is_err());
        let template = config.template("Iosevka");
        let mut again = Config::default();
        for line in template.lines() {
            again.apply_line(line).unwrap();
        }
        assert_eq!(again.font_family, None, "the family line is a commented-out example");
        assert_eq!(again.theme, "catppuccin-mocha");
    }
}
