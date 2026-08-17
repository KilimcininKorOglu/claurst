// image_paste.rs — Clipboard image detection and text paste via subprocess.
//
// Supports three operations:
//   1. `read_clipboard_text()` — read text from the system clipboard
//   2. `read_clipboard_image()` — detect an image in the clipboard and save to a temp file
//   3. Helper structs for image attachments shown in the prompt
//
// All clipboard access uses platform CLI tools (no native Rust bindings needed):
//   macOS  : pbpaste / osascript
//   Linux  : xclip / wl-paste
//   Windows: PowerShell Get-Clipboard

use std::path::PathBuf;
use std::process::Command;

// ---------------------------------------------------------------------------
// Image attachment state
// ---------------------------------------------------------------------------

/// A pasted image attachment waiting to be included in the next message.
#[derive(Debug, Clone)]
pub struct PastedImage {
    /// Path to the temporary PNG file on disk.
    pub path: PathBuf,
    /// Display label shown in the prompt (e.g. "clipboard.png" or "image.png").
    pub label: String,
    /// Original dimensions, if known.
    pub dimensions: Option<(u32, u32)>,
}

// ---------------------------------------------------------------------------
// Clipboard text reading
// ---------------------------------------------------------------------------

/// Read text from the system clipboard. Returns `None` if the clipboard is
/// empty, unavailable, or contains non-text data.
pub fn read_clipboard_text() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        read_text_macos()
    }
    #[cfg(target_os = "windows")]
    {
        read_text_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        read_text_linux()
    }
}

/// Read text from the primary selection when supported (Linux/X11/Wayland).
pub fn read_primary_text() -> Option<String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        read_primary_text_linux()
    }
}

#[cfg(target_os = "macos")]
fn read_text_macos() -> Option<String> {
    let out = Command::new("pbpaste").output().ok()?;
    if out.status.success() && !out.stdout.is_empty() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_text_linux() -> Option<String> {
    read_text_linux_selection(false)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_primary_text_linux() -> Option<String> {
    read_text_linux_selection(true)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_text_linux_selection(primary: bool) -> Option<String> {
    let commands: &[(&str, &[&str])] = if primary {
        &[
            ("wl-paste", &["--primary", "--no-newline"]),
            ("xclip", &["-selection", "primary", "-o"]),
            ("xsel", &["--primary", "--output"]),
        ]
    } else {
        &[
            ("wl-paste", &["--no-newline"]),
            ("xclip", &["-selection", "clipboard", "-o"]),
            ("xsel", &["--clipboard", "--output"]),
        ]
    };

    for (prog, args) in commands {
        if let Ok(out) = Command::new(prog).args(*args).output() {
            if out.status.success() && !out.stdout.is_empty() {
                return Some(String::from_utf8_lossy(&out.stdout).into_owned());
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn read_text_windows() -> Option<String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-Command", "Get-Clipboard"])
        .output()
        .ok()?;
    if out.status.success() && !out.stdout.is_empty() {
        Some(
            String::from_utf8_lossy(&out.stdout)
                .trim_end_matches('\n')
                .to_string(),
        )
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Clipboard image reading
// ---------------------------------------------------------------------------

/// Check whether the clipboard currently holds an image. If it does, write
/// the PNG to a temp file and return a `PastedImage`.
pub fn read_clipboard_image() -> Option<PastedImage> {
    #[cfg(target_os = "macos")]
    {
        read_image_macos()
    }
    #[cfg(target_os = "windows")]
    {
        read_image_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        read_image_linux()
    }
}

// ── macOS ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn read_image_macos() -> Option<PastedImage> {
    // Check whether the clipboard contains an image type
    let check = Command::new("osascript")
        .args(["-e", "the clipboard as «class PNGf»"])
        .output()
        .ok()?;

    if !check.status.success() || check.stdout.is_empty() {
        return None;
    }

    // Write the PNG bytes to a temp file
    let tmp = make_temp_png()?;

    let script = format!(
        r#"set pngData to (the clipboard as «class PNGf»)
set fp to open for access POSIX file "{}" with write permission
write pngData to fp
close access fp"#,
        tmp.display()
    );

    let write_out = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .ok()?;
    if write_out.status.success() && tmp.exists() && tmp.metadata().ok()?.len() > 0 {
        let dims = png_dimensions(&tmp);
        Some(PastedImage {
            label: "clipboard.png".to_string(),
            path: tmp,
            dimensions: dims,
        })
    } else {
        let _ = std::fs::remove_file(&tmp);
        None
    }
}

// ── Linux ──────────────────────────────────────────────────────────────────

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_image_linux() -> Option<PastedImage> {
    // Check whether clipboard contains an image type
    let has_image = check_linux_clipboard_has_image();
    if !has_image {
        return None;
    }

    let tmp = make_temp_png()?;

    // Try xclip then wl-paste
    let saved = try_save_linux_image(&tmp);
    if saved && tmp.exists() && tmp.metadata().ok()?.len() > 0 {
        let dims = png_dimensions(&tmp);
        Some(PastedImage {
            label: "clipboard.png".to_string(),
            path: tmp,
            dimensions: dims,
        })
    } else {
        let _ = std::fs::remove_file(&tmp);
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn check_linux_clipboard_has_image() -> bool {
    // xclip: list TARGETS and grep for image/
    if let Ok(out) = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
        .output()
    {
        if out.status.success() {
            let targets = String::from_utf8_lossy(&out.stdout);
            if targets.contains("image/") {
                return true;
            }
        }
    }
    // wl-paste: check available types
    if let Ok(out) = Command::new("wl-paste").args(["--list-types"]).output() {
        if out.status.success() {
            let types = String::from_utf8_lossy(&out.stdout);
            if types.contains("image/") {
                return true;
            }
        }
    }
    false
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn try_save_linux_image(path: &PathBuf) -> bool {
    // xclip
    if let Ok(out) = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "image/png", "-o"])
        .output()
    {
        if out.status.success()
            && !out.stdout.is_empty()
            && std::fs::write(path, &out.stdout).is_ok()
        {
            return true;
        }
    }
    // wl-paste
    if let Ok(out) = Command::new("wl-paste")
        .args(["--type", "image/png"])
        .output()
    {
        if out.status.success()
            && !out.stdout.is_empty()
            && std::fs::write(path, &out.stdout).is_ok()
        {
            return true;
        }
    }
    false
}

// ── Windows ────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn read_image_windows() -> Option<PastedImage> {
    // Check whether clipboard has an image
    let check = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "if ((Get-Clipboard -Format Image) -ne $null) { 'yes' } else { 'no' }",
        ])
        .output()
        .ok()?;

    let answer = String::from_utf8_lossy(&check.stdout).trim().to_string();
    if answer != "yes" {
        return None;
    }

    let tmp = make_temp_png()?;
    let tmp_str = tmp.display().to_string();

    let script = format!(
        "$img = Get-Clipboard -Format Image; \
         $img.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png)",
        tmp_str.replace('\'', "''")
    );

    let save = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .ok()?;

    if save.status.success() && tmp.exists() && tmp.metadata().ok()?.len() > 0 {
        let dims = png_dimensions(&tmp);
        Some(PastedImage {
            label: "clipboard.png".to_string(),
            path: tmp,
            dimensions: dims,
        })
    } else {
        let _ = std::fs::remove_file(&tmp);
        None
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Clipboard text writing
// ---------------------------------------------------------------------------

/// Write text to the system clipboard. Returns `true` on success.
pub fn write_clipboard_text(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    let native = write_text_macos_w(text);
    #[cfg(target_os = "windows")]
    let native = write_text_windows_w(text);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let native = write_text_linux_w(text);

    // Last resort. Over SSH, or on a headless host with no `wl-copy` or
    // `xclip`, nothing above can reach a clipboard, but the terminal on the
    // other end of the connection has one.
    native || osc52_write(text)
}

/// Longest text handed to the terminal through OSC 52.
///
/// xterm's default `maxStringParm` is 1000 bytes and other terminals cap the
/// sequence too. A payload past the cap is not truncated by the terminal, it is
/// discarded, so a silent partial copy is not a risk; the limit keeps a large
/// transcript from spending the write at all. Base64 costs four bytes per
/// three, so the source text bound is the sequence bound times 3/4.
const OSC52_MAX_TEXT_BYTES: usize = 72_000;

/// Hand text to the terminal emulator's clipboard with OSC 52.
///
/// The sequence is a terminal instruction, not screen content, so it is written
/// straight to stdout the same way `osc8::emit_hits` writes its hyperlinks; the
/// next frame repaints over nothing.
pub fn osc52_write(text: &str) -> bool {
    use std::io::{IsTerminal, Write};

    if text.is_empty() || text.len() > OSC52_MAX_TEXT_BYTES {
        return false;
    }
    // Only a terminal reads the sequence as an instruction. Piped stdout would
    // carry the escape bytes into whatever is reading the output.
    if !std::io::stdout().is_terminal() {
        return false;
    }
    let payload = osc52_sequence(text);
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(payload.as_bytes()).is_ok() && stdout.flush().is_ok()
}

/// Build the OSC 52 sequence for `text`, targeting the clipboard selection.
fn osc52_sequence(text: &str) -> String {
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, text);
    // BEL-terminated rather than ST: every terminal that speaks OSC 52 accepts
    // BEL, and some older ones never learned ST.
    format!("\x1b]52;c;{encoded}\x07")
}

#[cfg(target_os = "macos")]
fn write_text_macos_w(text: &str) -> bool {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = match Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn write_text_windows_w(text: &str) -> bool {
    use std::io::Write;
    use std::process::Stdio;
    // PowerShell Set-Clipboard reads from stdin via pipe
    let script =
        format!("[Console]::InputEncoding = [System.Text.Encoding]::UTF8; $input | Set-Clipboard");
    let mut child = match Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .stdin(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn write_text_linux_w(text: &str) -> bool {
    let clipboard_ok = write_text_linux_selection(text, false);
    let primary_ok = write_text_linux_selection(text, true);
    clipboard_ok || primary_ok
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn write_text_linux_selection(text: &str, primary: bool) -> bool {
    use std::io::Write;
    use std::process::Stdio;

    let commands: &[(&str, &[&str])] = if primary {
        &[
            ("wl-copy", &["--primary"]),
            ("xclip", &["-selection", "primary"]),
            ("xsel", &["--primary", "--input"]),
        ]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };

    for (prog, args) in commands {
        if let Ok(mut child) = Command::new(prog).args(*args).stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

fn make_temp_png() -> Option<PathBuf> {
    let tmp_dir = std::env::temp_dir();
    let name = format!(
        "claude-paste-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    Some(tmp_dir.join(name))
}

/// Read PNG dimensions from the IHDR chunk (bytes 16–23).
fn png_dimensions(path: &PathBuf) -> Option<(u32, u32)> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 24 {
        return None;
    }
    // PNG signature: 8 bytes; IHDR: 4 len + 4 type + 4 w + 4 h
    if &data[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    Some((w, h))
}

/// Read a file and base64-encode it for the Anthropic API.
pub fn encode_image_base64(path: &PathBuf) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    Some(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &data,
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_image_clone() {
        let img = PastedImage {
            path: PathBuf::from("/tmp/test.png"),
            label: "test.png".to_string(),
            dimensions: Some((800, 600)),
        };
        let cloned = img.clone();
        assert_eq!(cloned.label, "test.png");
        assert_eq!(cloned.dimensions, Some((800, 600)));
    }

    #[test]
    fn make_temp_png_produces_unique_names() {
        // Just check it returns a path under tmp with a .png suffix
        let p = make_temp_png().unwrap();
        assert!(p.to_string_lossy().contains("claude-paste-"));
        assert!(p.to_string_lossy().ends_with(".png"));
    }

    #[test]
    fn png_dimensions_invalid_data_returns_none() {
        let tmp = make_temp_png().unwrap();
        std::fs::write(&tmp, b"not a png").unwrap();
        assert!(png_dimensions(&tmp).is_none());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn png_dimensions_valid_header() {
        // Minimal valid PNG IHDR: 8-byte sig + 4-byte length + "IHDR" + 4-byte w + 4-byte h + ...
        let mut data = vec![0u8; 24];
        data[0..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        // IHDR chunk: length=13
        data[8..12].copy_from_slice(&13u32.to_be_bytes());
        data[12..16].copy_from_slice(b"IHDR");
        // width = 100
        data[16..20].copy_from_slice(&100u32.to_be_bytes());
        // height = 200
        data[20..24].copy_from_slice(&200u32.to_be_bytes());
        let tmp = make_temp_png().unwrap();
        std::fs::write(&tmp, &data).unwrap();
        let dims = png_dimensions(&tmp);
        assert_eq!(dims, Some((100, 200)));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn encode_image_base64_missing_file_returns_none() {
        let p = PathBuf::from("/nonexistent/file.png");
        assert!(encode_image_base64(&p).is_none());
    }

    #[test]
    fn encode_image_base64_roundtrip() {
        let tmp = make_temp_png().unwrap();
        std::fs::write(&tmp, b"hello world").unwrap();
        let b64 = encode_image_base64(&tmp).unwrap();
        let decoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64).unwrap();
        assert_eq!(decoded, b"hello world");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn the_osc52_sequence_carries_the_text_base64_encoded() {
        let sequence = osc52_sequence("hello world");

        assert!(sequence.starts_with("\u{1b}]52;c;"), "got {sequence:?}");
        assert!(
            sequence.ends_with('\u{7}'),
            "BEL terminates it, got {sequence:?}"
        );

        let body = sequence
            .trim_start_matches("\u{1b}]52;c;")
            .trim_end_matches('\u{7}');
        let decoded = match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body)
        {
            Ok(decoded) => decoded,
            Err(error) => panic!("the payload should decode: {error}"),
        };
        assert_eq!(decoded, b"hello world");
    }

    #[test]
    fn a_payload_past_the_terminal_cap_is_not_written() {
        // A terminal discards an over-long sequence rather than truncating it,
        // so reporting success would claim a copy that never happened.
        let oversized = "x".repeat(OSC52_MAX_TEXT_BYTES + 1);
        assert!(!osc52_write(&oversized));
        assert!(!osc52_write(""), "an empty copy is not a copy");
    }
}
