//! The credential verbs: `setup`, `password`, `root-password`, `ssh` and
//! `tokens`, through apid's local socket (`super::api`), so its checks, audit
//! and login backoff apply.

use anyhow::Result;
use serde_json::{Value, json};

use super::secret::{plain, secret};
use super::{api, json_reads, misuse};

const SETUP_USAGE: &str = "\
usage: mica setup [--hostname NAME] [--password-file FILE] [--device-code-file FILE]

  first-run setup: set the administrator password and, given one, the
  hostname. The password is asked for twice on the terminal unless
  --password-file names a file (- is stdin); a device whose claim asks for
  its device code is asked for it unless --device-code-file names one";

const PASSWORD_USAGE: &str = "\
usage: mica password change [--current-password-file FILE] [--password-file FILE]

  change the administrator password: the current one, then the new one
  twice, on the terminal unless a file names each (- is stdin, for one of
  them). A wrong current password counts in the login backoff";

const ROOT_PASSWORD_USAGE: &str = "\
usage: mica root-password [--password-file FILE]

  set the transient root password, until the next boot: asked twice on the
  terminal unless --password-file names a file (- is stdin)";

const SSH_USAGE: &str = "\
usage: mica ssh status [--json]
       mica ssh on|off
       mica ssh keys list [--json]
       mica ssh keys add <file|->
       mica ssh keys remove <fingerprint>

  status  whether SSH is on (access.ssh.enabled)
  on, off turn SSH on or off
  keys    the authorized keys: list them with their fingerprints, add one
          line from a file (- is stdin), remove one by its fingerprint";

const TOKENS_USAGE: &str = "\
usage: mica tokens list [--json]
       mica tokens create <name> [--json]
       mica tokens revoke <id>

  list    every API token: id, name, created
  create  mint a token named <name> and print it, once: only its digest is
          kept
  revoke  revoke the token <id>";

/// Flags that each take one value, in any order and at most once.
fn flags<'a>(
    words: &'a [String],
    names: &[&str],
    command: &str,
    usage: &str,
) -> Vec<Option<&'a str>> {
    let mut values = vec![None; names.len()];
    let mut rest = words.iter();
    while let Some(flag) = rest.next() {
        let Some(slot) = names.iter().position(|name| name == flag) else {
            misuse(command, &format!("unexpected argument: {flag}"), usage);
        };
        let Some(value) = rest.next() else {
            misuse(command, &format!("{flag} needs a value"), usage);
        };
        if values[slot].replace(value.as_str()).is_some() {
            misuse(command, &format!("{flag} is given twice"), usage);
        }
    }
    values
}

fn is_help(args: &[String]) -> bool {
    matches!(args, [word] if word == "help" || word == "--help")
}

/// `mica setup`.
pub fn setup(args: &[String], json: bool) -> Result<()> {
    if is_help(args) {
        println!("{SETUP_USAGE}");
        return Ok(());
    }
    let [hostname, password_file, code_file] = flags(
        args,
        &["--hostname", "--password-file", "--device-code-file"],
        "setup",
        SETUP_USAGE,
    )[..] else {
        unreachable!("three flags")
    };
    if password_file == Some("-") && code_file == Some("-") {
        misuse("setup", "only one of the files can be stdin", SETUP_USAGE);
    }
    json_reads(json, false, "setup", SETUP_USAGE);
    let session = api::get("/v1/session")?;
    if session["state"] != json!("setup") {
        return Err(super::Failure {
            exit: super::EXIT_FAILED,
            code: "already_configured".to_string(),
            message:
                "the device already has an administrator password; mica password change changes it"
                    .to_string(),
        }
        .into());
    }
    let mut body = json!({});
    if session["deviceCode"] == json!("required") {
        body["deviceCode"] = json!(match code_file {
            Some(file) => secret(Some(file), "Device code", false)?,
            None => plain("Device code (on the device's label)")?,
        });
    }
    body["password"] = json!(secret(password_file, "Administrator password", true)?);
    if let Some(hostname) = hostname {
        body["hostname"] = json!(hostname);
    }
    api::send("POST", "/v1/setup", &body)?;
    println!("the device is set up");
    Ok(())
}

/// `mica password change`.
pub fn password(args: &[String], json: bool) -> Result<()> {
    let rest = match args {
        [word] if word == "help" || word == "--help" => {
            println!("{PASSWORD_USAGE}");
            return Ok(());
        }
        [verb, rest @ ..] if verb == "change" => rest,
        [] => misuse("password", "name the command: change", PASSWORD_USAGE),
        [other, ..] => misuse(
            "password",
            &format!("unknown command: {other}"),
            PASSWORD_USAGE,
        ),
    };
    let [current_file, new_file] = flags(
        rest,
        &["--current-password-file", "--password-file"],
        "password",
        PASSWORD_USAGE,
    )[..] else {
        unreachable!("two flags")
    };
    if current_file == Some("-") && new_file == Some("-") {
        misuse(
            "password",
            "only one of the files can be stdin",
            PASSWORD_USAGE,
        );
    }
    json_reads(json, false, "password", PASSWORD_USAGE);
    let current = secret(current_file, "Current password", false)?;
    let new = secret(new_file, "New password", true)?;
    api::send(
        "POST",
        "/v1/actions/change-password",
        &json!({ "currentPassword": current, "newPassword": new }),
    )?;
    println!("the administrator password is changed");
    Ok(())
}

/// `mica root-password`.
pub fn root_password(args: &[String], json: bool) -> Result<()> {
    if is_help(args) {
        println!("{ROOT_PASSWORD_USAGE}");
        return Ok(());
    }
    let [file] = flags(
        args,
        &["--password-file"],
        "root-password",
        ROOT_PASSWORD_USAGE,
    )[..] else {
        unreachable!("one flag")
    };
    json_reads(json, false, "root-password", ROOT_PASSWORD_USAGE);
    let password = secret(file, "Root password", true)?;
    let accepted = api::send(
        "POST",
        "/v1/actions/transient-root-password",
        &json!({ "password": password }),
    )?;
    api::wait(&accepted)?;
    println!("the transient root password is set until the next boot");
    Ok(())
}

/// `mica ssh`.
pub fn ssh(args: &[String], json: bool) -> Result<()> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let reads = matches!(words.as_slice(), ["status"] | ["keys", "list"]);
    match words.as_slice() {
        [] | ["help" | "--help"] => {
            println!("{SSH_USAGE}");
            return Ok(());
        }
        ["status"] | ["on" | "off"] | ["keys", "list"] | ["keys", "add" | "remove", _] => {}
        _ => misuse("ssh", "unknown command or arguments", SSH_USAGE),
    }
    json_reads(json, reads, "ssh", SSH_USAGE);
    match words.as_slice() {
        ["status"] => {
            let enabled = api::get("/v1/settings/access.ssh.enabled")?;
            if json {
                println!("{}", json!({ "enabled": enabled }));
            } else {
                println!("enabled: {}", enabled == json!(true));
            }
        }
        [switch @ ("on" | "off")] => {
            let on = *switch == "on";
            api::wait(&api::send(
                "PUT",
                "/v1/settings/access.ssh.enabled",
                &json!(on),
            )?)?;
            println!("access.ssh.enabled is {on}");
        }
        ["keys", "list"] => {
            let list = api::get("/v1/ssh/authorized-keys")?;
            if json {
                println!("{list}");
            } else {
                print!("{}", render_keys(&list));
            }
        }
        ["keys", "add", file] => {
            let line = read_key(file)?;
            let added = api::send("POST", "/v1/ssh/authorized-keys", &json!({ "key": line }))?;
            println!(
                "added {}",
                added["key"]["fingerprint"].as_str().unwrap_or("the key")
            );
        }
        ["keys", "remove", fingerprint] => {
            api::delete(&format!(
                "/v1/ssh/authorized-keys/{}",
                api::segment(fingerprint)
            ))?;
            println!("removed {fingerprint}");
        }
        _ => unreachable!("parsed above"),
    }
    Ok(())
}

/// The one key line in `file` (`-` is stdin).
fn read_key(file: &str) -> Result<String> {
    let text = if file == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(
            &mut std::io::Read::take(std::io::stdin(), super::secret::MAX_SECRET + 1),
            &mut text,
        )?;
        text
    } else {
        std::fs::read_to_string(file).map_err(|error| anyhow::anyhow!("{file}: {error}"))?
    };
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    match lines.as_slice() {
        [line] => Ok((*line).to_string()),
        [] => anyhow::bail!("{file} holds no key"),
        _ => anyhow::bail!("{file} holds {} keys; add one at a time", lines.len()),
    }
}

fn render_keys(list: &Value) -> String {
    let mut out = String::new();
    for key in list["keys"].as_array().into_iter().flatten() {
        let kind = key["key"]
            .as_str()
            .and_then(|text| text.split_whitespace().next())
            .unwrap_or("?");
        out.push_str(&format!(
            "{} {kind} {}\n",
            key["fingerprint"].as_str().unwrap_or("(undecodable)"),
            key["comment"].as_str().unwrap_or("")
        ));
    }
    if out.is_empty() {
        out.push_str("no authorized keys\n");
    }
    out
}

/// `mica tokens`.
pub fn tokens(args: &[String], json: bool) -> Result<()> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["help" | "--help"] => {
            println!("{TOKENS_USAGE}");
            return Ok(());
        }
        ["list"] | ["create", _] | ["revoke", _] => {}
        _ => misuse("tokens", "unknown command or arguments", TOKENS_USAGE),
    }
    json_reads(
        json,
        matches!(words.as_slice(), ["list"] | ["create", _]),
        "tokens",
        TOKENS_USAGE,
    );
    match words.as_slice() {
        ["list"] => {
            let list = api::get("/v1/tokens")?;
            if json {
                println!("{list}");
            } else {
                let mut out = String::new();
                for token in list.as_array().into_iter().flatten() {
                    out.push_str(&format!(
                        "{} {} {}\n",
                        token["id"].as_str().unwrap_or("?"),
                        token["name"].as_str().unwrap_or(""),
                        token["created"]
                    ));
                }
                print!("{}", if out.is_empty() { "no tokens\n" } else { &out });
            }
        }
        ["create", name] => {
            let minted = api::send("POST", "/v1/tokens", &json!({ "name": name }))?;
            if json {
                println!("{minted}");
            } else {
                println!("{}", minted["token"].as_str().unwrap_or_default());
                eprintln!(
                    "token {} ({name}): shown once, keep it now",
                    minted["id"].as_str().unwrap_or("?")
                );
            }
        }
        ["revoke", id] => {
            api::delete(&format!("/v1/tokens/{}", api::segment(id)))?;
            println!("revoked {id}");
        }
        _ => unreachable!("parsed above"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_file_holds_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("key.pub");
        let path = file.to_str().unwrap();
        std::fs::write(&file, "# mine\nssh-ed25519 AAAA me@host\n\n").unwrap();
        assert_eq!(read_key(path).unwrap(), "ssh-ed25519 AAAA me@host");
        std::fs::write(&file, "ssh-ed25519 AAAA a\nssh-ed25519 BBBB b\n").unwrap();
        assert!(read_key(path).is_err());
        std::fs::write(&file, "\n").unwrap();
        assert!(read_key(path).is_err());
    }

    #[test]
    fn the_keys_are_listed_by_fingerprint() {
        let list = json!({ "keys": [
            { "key": "ssh-ed25519 AAAA", "comment": "me@host", "fingerprint": "SHA256:abc" },
            { "key": "ssh-rsa BBBB", "fingerprint": null },
        ], "notice": "..." });
        assert_eq!(
            render_keys(&list),
            "SHA256:abc ssh-ed25519 me@host\n(undecodable) ssh-rsa \n"
        );
        assert_eq!(render_keys(&json!({ "keys": [] })), "no authorized keys\n");
    }
}
