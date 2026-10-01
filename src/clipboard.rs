//! Clipboard and browser hand-off. Prefers the platform's own tools, which
//! keep working after sessy exits (an X11 selection owned by sessy itself
//! would vanish with it), and falls back to the OSC 52 terminal escape, which
//! also reaches the local clipboard over SSH.

use std::io::Write;
use std::process::{Command, Stdio};

/// Put `text` on the system clipboard.
pub fn copy(text: &str) -> Result<(), String> {
    let remote = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    // Over SSH the host's clipboard tools copy to the wrong machine.
    if remote && osc52(text).is_ok() {
        return Ok(());
    }
    for (program, args) in clipboard_tools() {
        if pipe_to(program, args, text).is_ok() {
            return Ok(());
        }
    }
    if copypasta_copy(text).is_ok() {
        return Ok(());
    }
    osc52(text).map_err(|e| format!("no clipboard available: {}", e))
}

fn clipboard_tools() -> Vec<(&'static str, &'static [&'static str])> {
    let mut tools: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if cfg!(target_os = "macos") {
        tools.push(("pbcopy", &[]));
    } else if cfg!(windows) {
        // `clip.exe` mangles UTF-8; copypasta talks to the Win32 API instead.
    } else {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            tools.push(("wl-copy", &[]));
        }
        if std::env::var_os("DISPLAY").is_some() {
            tools.push(("xclip", &["-selection", "clipboard"]));
            tools.push(("xsel", &["--clipboard", "--input"]));
        }
    }
    tools
}

/// Read the system clipboard as text. Used when Ctrl+V/⌘V reaches sessy as a
/// key instead of the terminal pasting (bracketed paste) on its own.
pub fn paste() -> Result<String, String> {
    for (program, args) in paste_tools() {
        if let Ok(text) = read_from(program, args) {
            return Ok(text);
        }
    }
    copypasta_paste().map_err(|e| format!("no clipboard available: {}", e))
}

fn paste_tools() -> Vec<(&'static str, &'static [&'static str])> {
    let mut tools: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if cfg!(target_os = "macos") {
        tools.push(("pbpaste", &[]));
    } else if cfg!(windows) {
        // copypasta reads the Win32 clipboard directly.
    } else {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            tools.push(("wl-paste", &["--no-newline", "--type", "text"]));
        }
        if std::env::var_os("DISPLAY").is_some() {
            tools.push(("xclip", &["-selection", "clipboard", "-o"]));
            tools.push(("xsel", &["--clipboard", "--output"]));
        }
    }
    tools
}

fn read_from(program: &str, args: &[&str]) -> std::io::Result<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(std::io::Error::other(format!("{} exited with {}", program, out.status)))
    }
}

fn copypasta_paste() -> Result<String, String> {
    use copypasta::ClipboardProvider;
    let mut ctx = copypasta::ClipboardContext::new().map_err(|e| e.to_string())?;
    ctx.get_contents().map_err(|e| e.to_string())
}

fn pipe_to(program: &str, args: &[&str], text: &str) -> std::io::Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("{} exited with {}", program, status)))
    }
}

fn copypasta_copy(text: &str) -> Result<(), String> {
    use copypasta::ClipboardProvider;
    let mut ctx = copypasta::ClipboardContext::new().map_err(|e| e.to_string())?;
    ctx.set_contents(text.to_string()).map_err(|e| e.to_string())
}

/// Ask the terminal to set the clipboard (OSC 52). Written to the controlling
/// terminal so `--print`'s captured stdout stays clean.
fn osc52(text: &str) -> std::io::Result<()> {
    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    #[cfg(unix)]
    {
        if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            tty.write_all(seq.as_bytes())?;
            return tty.flush();
        }
    }
    let mut err = std::io::stderr();
    err.write_all(seq.as_bytes())?;
    err.flush()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Open `url` in the default browser.
pub fn open_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("not a web URL".to_string());
    }
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(windows) {
        ("explorer", vec![url])
    } else {
        ("xdg-open", vec![url])
    };
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {}", program, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64("κ".as_bytes()), "zro=");
    }

    #[test]
    fn test_open_url_rejects_non_web_urls() {
        assert!(open_url("file:///etc/passwd").is_err());
        assert!(open_url("--help").is_err());
    }
}
