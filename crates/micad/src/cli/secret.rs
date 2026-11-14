//! Secrets on the command line: never from argv. From the terminal with its
//! echo off, or from a file (`-` for standard input) for a script.

use std::io::{BufRead, Read, Write};

use anyhow::{Context, Result, bail};

/// The largest secret file read.
pub const MAX_SECRET: u64 = 4096;

/// The secret from `file`, or asked for as `label` on the terminal with its
/// echo off, twice when `confirm`. The file's one line is the secret, without
/// its line end.
pub fn secret(file: Option<&str>, label: &str, confirm: bool) -> Result<String> {
    let text = match file {
        Some("-") => read_limited(std::io::stdin().lock())?,
        Some(file) => {
            read_limited(std::fs::File::open(file).with_context(|| format!("open {file}"))?)
                .with_context(|| format!("read {file}"))?
        }
        None => {
            let first = ask(label, false)?;
            if confirm && ask(&format!("{label} again"), false)? != first {
                bail!("the two entries differ");
            }
            first
        }
    };
    let secret = line(&text);
    if secret.is_empty() {
        bail!("no {} given", label.to_lowercase());
    }
    Ok(secret.to_string())
}

/// A value that is not secret, as `label` on the terminal with its echo on.
pub fn plain(label: &str) -> Result<String> {
    Ok(line(&ask(label, true)?).trim().to_string())
}

/// `text` without one line end.
fn line(text: &str) -> &str {
    text.strip_suffix('\n')
        .map_or(text, |line| line.strip_suffix('\r').unwrap_or(line))
}

fn read_limited(reader: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader.take(MAX_SECRET + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SECRET {
        bail!("longer than {MAX_SECRET} bytes");
    }
    String::from_utf8(bytes).context("not UTF-8")
}

/// One line from the terminal after `label: `, with its echo on or off.
fn ask(label: &str, echo: bool) -> Result<String> {
    let stdin = std::io::stdin();
    if !rustix::termios::isatty(&stdin) {
        bail!(
            "standard input is not a terminal; give the {} in a file",
            label.to_lowercase()
        );
    }
    print!("{label}: ");
    std::io::stdout().flush()?;
    let saved = rustix::termios::tcgetattr(&stdin)?;
    if !echo {
        let mut quiet = saved.clone();
        quiet.local_modes.remove(rustix::termios::LocalModes::ECHO);
        rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &quiet)?;
    }
    let mut line = String::new();
    let read = stdin.lock().take(MAX_SECRET + 1).read_line(&mut line);
    if !echo {
        rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &saved)?;
        println!();
    }
    read?;
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_file_is_one_line_of_bounded_text() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("password");
        for (text, want) in [
            ("secret\n", Some("secret")),
            ("secret\r\n", Some("secret")),
            ("with spaces ", Some("with spaces ")),
            ("\n", None),
            ("", None),
        ] {
            std::fs::write(&file, text).unwrap();
            let read = secret(Some(file.to_str().unwrap()), "Password", false);
            assert_eq!(read.ok().as_deref(), want, "{text:?}");
        }
        std::fs::write(&file, "x".repeat(4097)).unwrap();
        assert!(secret(Some(file.to_str().unwrap()), "Password", false).is_err());
    }
}
