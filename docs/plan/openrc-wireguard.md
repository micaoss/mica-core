# OpenRC WireGuard: micad runs the tunnels over netlink

Status: approved plan (repair-20261110). It was approved by the user on 2026-11-11 as written, and nothing here is implemented yet. It comes after WG-01 round 2 in the approved order.

**Decision:**

- On an OpenRC root, micad runs WireGuard tunnels itself, over netlink, with no new Base package.
- systemd roots keep networkd, as today.
- The work is done in 3 rounds. The OpenRC refusal of `kind: wireguard` is lifted in round 3, once the QEMU OpenRC case passes.
- Targets:
  - **mini-x64**: its kernel has `CONFIG_WIREGUARD=y` already;
  - **qemu-armhf** and **ssd202d**: mica-build adds `wireguard` to their `KERNEL_FEATURES`. Both have it unset today, along with the CURVE25519 and CHACHA20POLY1305 libraries.

## 1. Link lifecycle

- A netlink reconciler in micad. It is used only when the root has no networkd.
- For each `kind: wireguard` entry, it does the following:
  - `RTM_NEWLINK` with kind `wireguard`;
  - generic netlink `WG_CMD_SET_DEVICE`, with the private key from the Keystore, the listen port and the peers;
  - `RTM_NEWADDR` with the static address;
  - link up.
- A peer change is applied in place, per peer: add, update, or remove with `WGPEER_F_REMOVE_ME`. The device is not rebuilt, so the other peers keep their handshakes.
- An entry that is removed or disabled deletes the link.

## 2. Route ownership

- The routes are the same set as `wireguard_routes(cfg)`, so the rules from WG-01 round 1 hold unchanged.
- Each route is installed with `RTM_NEWROUTE` on the wg device, with a Mica-specific `rtm_protocol` value and `PreferredSource`.
- Ownership is the pair (device, protocol).
- Each reconcile dumps the routes, adds the missing ones, and deletes only the stale routes that micad owns. Nothing with another protocol is touched.
- Deleting the link drops the rest.

## 3. Endpoints

- The kernel takes numeric endpoints only, so micad resolves a host name itself. Resolution is bounded, and retried at 5, 15, 30 and 60 s.
- Until an endpoint resolves, the peer reports `pending_endpoint_resolution` in the observations.
- A resolved address is checked against the endpoint loop rule again.

## 4. Boot and restart recovery

- Links do not persist across a reboot. After boot, micad creates them from the settings at startup, before it reports the network ready.
- On a micad restart, it reconciles idempotently against the kernel state:
  - it reads the existing link, address, peers and owned routes;
  - it fixes only the differences;
  - it never recreates a healthy tunnel.
- ifupdown's `/etc/network/interfaces` never lists wg devices.

## 5. Tests

- Golden byte tests for the netlink messages: link, set device, peer remove, and route add and delete with the protocol.
- Reconciler tests over a fake netlink trait:
  - the first apply;
  - a peer edit in place;
  - a stale owned route deleted;
  - a foreign route left alone;
  - a restart without recreating the tunnel.
- A QEMU OpenRC case with two network namespaces:
  - a handshake;
  - a route to the peer subnet;
  - a peer edit, with the other peer still up;
  - recovery after a reboot and after a micad restart.

## 6. Rounds

1. The netlink messages and the reconciler, with unit tests.
2. Endpoint resolution and the observations.
3. The QEMU OpenRC case and the docs. The OpenRC refusal of `kind: wireguard` is lifted here.

## Cross-repo

- mica-build: add `wireguard` to `KERNEL_FEATURES` for qemu-armhf and ssd202d. This is routed by the master.
- The board kernels on NAND (ssd202d, qemu-armhf) grow by the WireGuard module and its crypto libraries. mica-build owns that size budget.
