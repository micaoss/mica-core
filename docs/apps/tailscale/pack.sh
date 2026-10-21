#!/bin/sh
# usage: pack.sh <version> <amd64|arm64|armhf>
# Builds tailscale-<version>-<arch>.tar.gz, a mica app bundle, in the current
# directory from Tailscale's static release. Needs curl, tar and jq.
set -eu
version=$1
arch=$2
case $arch in
	amd64 | arm64) upstream=$arch ;;
	armhf) upstream=arm ;;
	*) echo "pack.sh: unknown arch $arch" >&2; exit 2 ;;
esac
here=$(cd "$(dirname "$0")" && pwd)
out=$PWD/tailscale-$version-$arch.tar.gz
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

curl -fsSL "https://pkgs.tailscale.com/stable/tailscale_${version}_${upstream}.tgz" |
	tar -xzf - -C "$work"
mkdir -p "$work/app/files" "$work/app/units/systemd" "$work/app/units/openrc" "$work/app/hooks"
cp "$work/tailscale_${version}_${upstream}/tailscale" "$work/tailscale_${version}_${upstream}/tailscaled" "$work/app/files/"
cp "$here/tailscaled.service" "$work/app/units/systemd/"
cp "$here/tailscaled.openrc" "$work/app/units/openrc/tailscaled"
install -m 0755 "$here/first-run" "$work/app/hooks/first-run"
jq --arg version "$version" --arg arch "$arch" '.version = $version | .arch = [$arch]' \
	"$here/mica-app.json" >"$work/app/mica-app.json"

tar -czf "$out" --owner=0 --group=0 -C "$work/app" mica-app.json files units hooks
echo "$out"
