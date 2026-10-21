//! The units micad writes for the services of an app that runs as its own
//! user. Each runs through `setpriv` as the app's uid, keeping only the
//! capabilities the app declares, with no new privileges; under systemd in a
//! sandbox besides, where it writes only its data directory.

use std::collections::BTreeMap;

use anyhow::Context as _;
use micad_settings::Init;

use super::manifest::{self, Service};

/// Where `setpriv` is on a device.
pub const SETPRIV: &str = "/usr/bin/setpriv";

/// Who an app's processes run as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Its uid, which is its gid too.
    pub uid: u32,
    /// The groups of the devices it is given.
    pub groups: Vec<u32>,
    /// `CAP_*` names it keeps.
    pub capabilities: Vec<String>,
}

/// `setpriv`'s arguments for `account`, through the `--` before the command.
pub fn setpriv_args(account: &Account) -> Vec<String> {
    let caps = std::iter::once("-all".to_string())
        .chain(
            account
                .capabilities
                .iter()
                .map(|cap| format!("+{}", cap.trim_start_matches("CAP_").to_ascii_lowercase())),
        )
        .collect::<Vec<_>>()
        .join(",");
    let groups = if account.groups.is_empty() {
        "--clear-groups".to_string()
    } else {
        let list: Vec<String> = account.groups.iter().map(u32::to_string).collect();
        format!("--groups={}", list.join(","))
    };
    vec![
        format!("--reuid={}", account.uid),
        format!("--regid={}", account.uid),
        groups,
        format!("--bounding-set={caps}"),
        format!("--inh-caps={caps}"),
        format!("--ambient-caps={caps}"),
        "--no-new-privs".to_string(),
        "--".to_string(),
    ]
}

/// `value`, refused when it carries what a unit file or a shell would read
/// as more than text. The manifest's rules refuse these already; a unit is
/// never written past them.
fn plain(value: &str) -> anyhow::Result<&str> {
    anyhow::ensure!(
        !value
            .chars()
            .any(|c| matches!(c, '"' | '\'' | '\\' | '`' | '$') || c.is_control()),
        "{value:?} is not plain text"
    );
    Ok(value)
}

/// What a service of `app` runs: setpriv, then its file and arguments with
/// `vars` expanded.
fn command(
    service: &Service,
    account: &Account,
    vars: &BTreeMap<String, String>,
) -> anyhow::Result<Vec<String>> {
    let dir = vars.get("APP_DIR").context("APP_DIR")?;
    let mut exec = service.exec.iter();
    let file = exec.next().context("the service runs nothing")?;
    let mut args = vec![SETPRIV.to_string()];
    args.extend(setpriv_args(account));
    args.push(format!("{dir}/{file}"));
    for arg in exec {
        args.push(manifest::expand(arg, vars).with_context(|| format!("expand {arg:?}"))?);
    }
    Ok(args)
}

/// The unit file or script of `service` for `init`.
///
/// # Errors
///
/// An argument names a variable `vars` lacks.
pub fn unit(
    init: Init,
    app: &str,
    service: &Service,
    account: &Account,
    devices: &[String],
    vars: &BTreeMap<String, String>,
) -> anyhow::Result<String> {
    let args = command(service, account, vars)?;
    for value in args
        .iter()
        .chain(vars.keys())
        .chain(vars.values())
        .chain(devices)
    {
        plain(value)?;
    }
    let data = vars.get("APP_DATA").context("APP_DATA")?;
    let app_dir = format!("/mica/apps/{app}");
    Ok(match init {
        Init::Systemd => {
            let exec: Vec<String> = args
                .iter()
                .map(|arg| format!("\"{}\"", arg.replace('%', "%%")))
                .collect();
            let mut caps = vec!["CAP_SETUID", "CAP_SETGID", "CAP_SETPCAP"];
            caps.extend(account.capabilities.iter().map(String::as_str));
            let mut text = format!(
                "# Written by micad for app {app}: setpriv runs it as uid {uid}.\n\
                 [Unit]\n\
                 Description={app}: {name}\n\
                 Wants=network-online.target\n\
                 After=network-online.target\n\
                 RequiresMountsFor={app_dir} {data}\n\
                 \n\
                 [Service]\n\
                 EnvironmentFile={app_dir}/app.env\n\
                 Environment=\"HOME={data}\"\n\
                 WorkingDirectory={data}\n\
                 ExecStart={exec}\n\
                 SyslogIdentifier={name}\n\
                 Restart=on-failure\n\
                 RestartSec=5\n\
                 NoNewPrivileges=yes\n\
                 ProtectSystem=strict\n\
                 ReadWritePaths={data}\n\
                 PrivateTmp=yes\n\
                 ProtectHome=yes\n\
                 ProtectKernelTunables=yes\n\
                 ProtectKernelModules=yes\n\
                 ProtectControlGroups=yes\n\
                 RestrictSUIDSGID=yes\n\
                 CapabilityBoundingSet={caps}\n\
                 DevicePolicy=closed\n",
                uid = account.uid,
                name = service.name,
                exec = exec.join(" "),
                caps = caps.join(" "),
            );
            for device in devices {
                text.push_str(&format!("DeviceAllow={device} rw\n"));
            }
            text.push_str("\n[Install]\nWantedBy=multi-user.target\n");
            text
        }
        Init::Openrc => {
            let quote = |arg: &str| format!("'{arg}'");
            let mut daemon = vec![format!("--chdir {}", quote(data))];
            daemon.extend(
                std::iter::once(("HOME", data.as_str()))
                    .chain(vars.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                    .map(|(key, value)| format!("-e {}", quote(&format!("{key}={value}")))),
            );
            let rest: Vec<String> = args[1..].iter().map(|arg| quote(arg)).collect();
            format!(
                "#!/sbin/openrc-run\n\
                 # Written by micad for app {app}: setpriv runs it as uid {uid}.\n\
                 \n\
                 description=\"{app}: {name}\"\n\
                 supervisor=supervise-daemon\n\
                 command={SETPRIV}\n\
                 command_args=\"{rest}\"\n\
                 supervise_daemon_args=\"{daemon}\"\n\
                 \n\
                 depend() {{\n\
                 \tneed mica-mounts\n\
                 \tafter net\n\
                 }}\n",
                uid = account.uid,
                name = service.name,
                rest = rest.join(" "),
                daemon = daemon.join(" "),
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tailscaled() -> (Service, Account, BTreeMap<String, String>) {
        let service = Service {
            name: "tailscaled".into(),
            exec: vec![
                "files/tailscaled".into(),
                "--state=${APP_DATA}/tailscaled.state".into(),
                "--socket=${TS_SOCKET}".into(),
                "--port=41641".into(),
            ],
            enable: true,
        };
        let account = Account {
            uid: 30000,
            groups: Vec::new(),
            capabilities: vec!["CAP_NET_ADMIN".into(), "CAP_NET_RAW".into()],
        };
        let vars = [
            ("APP_NAME", "tailscale"),
            ("APP_VERSION", "1.104.2"),
            ("APP_DIR", "/mica/apps/tailscale/current"),
            ("APP_DATA", "/srv/apps/tailscale"),
            ("TS_SOCKET", "/srv/apps/tailscale/tailscaled.sock"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        (service, account, vars)
    }

    #[test]
    fn setpriv_keeps_only_what_is_declared() {
        let (_, mut account, _) = tailscaled();
        assert_eq!(
            setpriv_args(&account),
            [
                "--reuid=30000",
                "--regid=30000",
                "--clear-groups",
                "--bounding-set=-all,+net_admin,+net_raw",
                "--inh-caps=-all,+net_admin,+net_raw",
                "--ambient-caps=-all,+net_admin,+net_raw",
                "--no-new-privs",
                "--",
            ]
        );
        account.capabilities.clear();
        account.groups = vec![20, 44];
        let args = setpriv_args(&account);
        assert_eq!(args[2], "--groups=20,44");
        assert_eq!(args[3], "--bounding-set=-all");
    }

    #[test]
    fn a_systemd_service_is_sandboxed_and_runs_as_the_app() {
        let (service, account, vars) = tailscaled();
        let text = unit(
            Init::Systemd,
            "tailscale",
            &service,
            &account,
            &["/dev/net/tun".into()],
            &vars,
        )
        .unwrap();
        for line in [
            "ExecStart=\"/usr/bin/setpriv\" \"--reuid=30000\" \"--regid=30000\" \"--clear-groups\" \"--bounding-set=-all,+net_admin,+net_raw\" \"--inh-caps=-all,+net_admin,+net_raw\" \"--ambient-caps=-all,+net_admin,+net_raw\" \"--no-new-privs\" \"--\" \"/mica/apps/tailscale/current/files/tailscaled\" \"--state=/srv/apps/tailscale/tailscaled.state\" \"--socket=/srv/apps/tailscale/tailscaled.sock\" \"--port=41641\"",
            "SyslogIdentifier=tailscaled",
            "NoNewPrivileges=yes",
            "ProtectSystem=strict",
            "ReadWritePaths=/srv/apps/tailscale",
            "PrivateTmp=yes",
            "RestrictSUIDSGID=yes",
            "CapabilityBoundingSet=CAP_SETUID CAP_SETGID CAP_SETPCAP CAP_NET_ADMIN CAP_NET_RAW",
            "DevicePolicy=closed",
            "DeviceAllow=/dev/net/tun rw",
            "Environment=\"HOME=/srv/apps/tailscale\"",
            "WantedBy=multi-user.target",
        ] {
            assert!(text.lines().any(|l| l == line), "{line}\n{text}");
        }
    }

    #[test]
    fn a_value_that_is_not_plain_is_never_written() {
        let (service, account, mut vars) = tailscaled();
        vars.insert("TS_SOCKET".into(), "/x\"; touch /pwn; \"".into());
        for init in [Init::Systemd, Init::Openrc] {
            assert!(unit(init, "tailscale", &service, &account, &[], &vars).is_err());
        }
    }

    #[test]
    fn a_percent_is_not_a_specifier() {
        let (mut service, account, vars) = tailscaled();
        service.exec.push("--hostname=a%n".into());
        let text = unit(Init::Systemd, "tailscale", &service, &account, &[], &vars).unwrap();
        assert!(text.contains("\"--hostname=a%%n\""));
    }

    #[test]
    fn an_openrc_service_runs_under_supervise_daemon_as_the_app() {
        let (service, account, vars) = tailscaled();
        let text = unit(Init::Openrc, "tailscale", &service, &account, &[], &vars).unwrap();
        assert!(text.starts_with("#!/sbin/openrc-run\n"));
        for line in [
            "supervisor=supervise-daemon",
            "command=/usr/bin/setpriv",
            "command_args=\"'--reuid=30000' '--regid=30000' '--clear-groups' '--bounding-set=-all,+net_admin,+net_raw' '--inh-caps=-all,+net_admin,+net_raw' '--ambient-caps=-all,+net_admin,+net_raw' '--no-new-privs' '--' '/mica/apps/tailscale/current/files/tailscaled' '--state=/srv/apps/tailscale/tailscaled.state' '--socket=/srv/apps/tailscale/tailscaled.sock' '--port=41641'\"",
        ] {
            assert!(text.lines().any(|l| l == line), "{line}\n{text}");
        }
        assert!(text.contains("--chdir '/srv/apps/tailscale' -e 'HOME=/srv/apps/tailscale' -e 'APP_DATA=/srv/apps/tailscale'"));
    }
}
