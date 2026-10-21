# Tailscale as a mica app

`docs/apps/tailscale/` builds Tailscale's static release into an app bundle (`docs/mica-core.md`
section 3.11): `tailscaled` as a unit for either init, its state in `/srv/apps/tailscale`. A test
in `crates/micad/src/apps/bundle.rs` unpacks these files as a bundle under both inits.

| File | In the bundle |
| --- | --- |
| `mica-app.json` | `mica-app.json`, with the version and arch `pack.sh` is given |
| `tailscaled.service` | `units/systemd/tailscaled.service` |
| `tailscaled.openrc` | `units/openrc/tailscaled` |
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
/mica/apps/tailscale/current/files/tailscale up
```

`tailscale up` prints the login URL. The node's state is `/srv/apps/tailscale/tailscaled.state`;
it stays across upgrades and rollbacks, and `micad app remove tailscale --purge` deletes it.
The systemd unit reads `/mica/apps/tailscale/app.env` for `APP_DATA` and `TS_SOCKET`; the OpenRC
script names the same paths. `tailscaled` needs `/dev/net/tun`.

A later version is a new bundle: `micad app install` (or `upgrade`) on it switches to it and keeps
this one for `micad app rollback tailscale`. To have micad find it, add `"update": {"url":
"https://.../tailscale.json"}` to the manifest and publish a version file there, as section 3.11
describes.
