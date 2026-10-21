# Tailscale as a mica app

`docs/apps/tailscale/` builds Tailscale's static release into an app bundle (`docs/mica-core.md`
section 3.11). `tailscaled` runs as the app's own user, keeping `CAP_NET_ADMIN` and `CAP_NET_RAW`
and opening `/dev/net/tun`, with its state in `/srv/apps/tailscale`; micad writes its unit for
either init from the manifest. A test in `crates/micad/src/apps/bundle.rs` unpacks these files as
a bundle under both inits.

| File | In the bundle |
| --- | --- |
| `mica-app.json` | `mica-app.json`, with the version and arch `pack.sh` is given |
| `first-run` | `hooks/first-run` |
| Tailscale's `tailscale`, `tailscaled` | `files/` |

## Build

```sh
sh docs/apps/tailscale/pack.sh 1.104.2 arm64   # amd64, arm64 or armhf
```

It fetches `https://pkgs.tailscale.com/stable/tailscale_<version>_<arch>.tgz` and writes
`tailscale-<version>-<arch>.tar.gz` in the current directory.

## Install

Upload the bundle on the console's services page, or on the device:

```sh
micad app install tailscale-1.104.2-arm64.tar.gz
/mica/apps/tailscale/current/files/tailscale --socket=/srv/apps/tailscale/tailscaled.sock up
```

`tailscale up` prints the login URL. The node's state and its socket are in
`/srv/apps/tailscale`; they stay across upgrades and rollbacks, and `micad app remove tailscale
--purge` deletes them. Without `iptables` on the root, `tailscaled` sets its firewall rules
through nftables. The kernel needs `CONFIG_TUN`.

A later version is a new bundle: `micad app install` (or `upgrade`) on it switches to it and keeps
this one for `micad app rollback tailscale`. To have micad find it, add `"update": {"url":
"https://.../tailscale.json"}` to the manifest and publish a version file there, as section 3.11
describes.
