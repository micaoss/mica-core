# Changelog

## 2026-09-26 10:40 [feature]

The product's features decide which API a device serves
(20260926-1000-features-decide-the-surface):

- `micad_settings::Features` reads `FEATURES` from `/usr/lib/mica/product.conf`, the dm-verity
  root's statement of the product. mica-core acts on `wifi`, `bluetooth`, `ssh`, `containers` and
  `mqtt`; everything else is always present. No product file, or no `FEATURES` line, serves every
  feature.
- micad registers only the reconcilers of the features the product carries, refuses a settings
  write that would change another feature's subtree as `feature not in this product` (not found on
  the bus), refuses that feature's members by the same name, and registers no Bluetooth pairing
  agent without `bluetooth`.
- apid mounts only those features' routes; the rest answer 404 `not_found`, authenticated or not.
  `GET /api/v1/meta` lists `features`, and `openapi.json` names the feature of every gated
  operation.
- The console hides the Wi-Fi and Bluetooth tabs, the container and MQTT services and the SSH
  section of a feature `meta` does not list.
- The transient root password stays available without `ssh`: it also opens the physical console.
- mica-build lists `ssh` in `FEATURES` for a product that carries `mica-ssh`, in the commit that
  pins this release; a product that does not loses the SSH API.
- Comments in the code describe behaviour only: references to plans, tasks, decisions, design
  documents and dates are gone, and so is history narrated as "used to" and "no longer".
- Versions: the micad producer (`micad`, `mica-apid`, `mica-apid-ui`) `0.1.0-6`; the mqtt producer
  `0.1.0-5`; `mica-deploy` and `mica-lifecycle` `0.1.0-7`; `mica-sftp-server` `0.1.0-3` -- the
  workspace manifest is an input to every producer.

## 2026-09-26 09:20 [fix]

The Wi-Fi client joins WPA3 networks:

- A passphrase network was rendered `key_mgmt=WPA-PSK` alone, so a WPA3-only access point --
  mandatory on 6 GHz (Wi-Fi 6E/7), and the default on more new routers -- could not be joined,
  whatever wpa_supplicant build the image carried. It is `key_mgmt=WPA-PSK SAE` with
  `ieee80211w=1` now: one block joins WPA2, WPA3 and transition-mode access points, with PMF
  offered, which SAE requires.
- `sae_pwe=2` in the global section admits SAE's hash-to-element as well as hunting-and-pecking.
  The default is the second alone, and WPA3 on 6 GHz requires the first, so SAE without it still
  failed there.
- A network stored as a raw 64-hex PMK stays `key_mgmt=WPA-PSK`: SAE derives its keys from the
  password itself, which a PMK does not carry.
- The access point is unchanged (802.11g, WPA2): it serves provisioning. No version bump: `micad`
  is the unreleased `0.1.0-5`.

## 2026-09-26 08:55 [change]

One management executable, an optional console, stripped binaries
(20260926-0841-one-binary-and-an-optional-console):

- **Every executable is stripped** (`[profile.release] strip = "symbols"`). Only `mica-runkit` was;
  the symbol tables were 23% of `micad` and of `mica-apid`. A device backtrace is resolved
  against an unstripped build of the same commit.
- **The console is a package, `mica-apid-ui`,** at `/usr/share/mica-apid/ui` in the dm-verity root,
  no longer compiled into apid: `build.rs` and `MICA_APID_UI_DIST_DIR` are gone, and apid and the
  Rust gate compile without Bun. apid indexes the tree once at start (regular files, safe logical
  paths, bounded, an `index.html`) and serves `/_ui/` from the index alone; `APID_UI_DIR` moves
  it. A product without the package is API-only: `/_ui/` and `/` answer 404, the API is unchanged,
  and a console tree that breaks a rule is logged and served as no console.
- **`mica-apid` is `micad` under another name.** `micad` runs apid's entry point when started as
  `mica-apid` and is micad under every other name; `/usr/bin/mica-apid` is a symlink. The
  2026-09-13 split ("so an API upgrade never repacks micad") never bought independence --
  `mica-apid` has always depended on `micad` at exactly its version -- and linked tokio, zbus,
  serde and the settings crate twice. The units and their sandboxing are unchanged.
- **One producer, three packages:** `pkgs/micad` ships `micad`, `mica-apid` (the symlink, the unit,
  the OpenAPI document) and `mica-apid-ui`; `pkgs/apid` is gone.
- Measured against `20260926-0815` (amd64, installed): `micad` + `mica-apid` 29.0 MB before;
  18.4 MB API-only and 19.7 MB with the console after (`micad` 18.06 MB, `openapi.json` 0.31 MB,
  console 1.31 MB). The archives go from 6.83 MB to 5.23 MB.
- Versions: `micad`, `mica-apid` and `mica-apid-ui` `0.1.0-5` (`mica-apid` is released at
  `0.1.0-4` and never goes back); the mqtt producer `0.1.0-4` for the `micad (= 0.1.0-5)` pin;
  `mica-deploy` and `mica-lifecycle` `0.1.0-6` and `mica-sftp-server` `0.1.0-2`, because the
  release profile strips their binaries too.

## 2026-09-26 06:52 [change]

The device reads its board from the signed boot policy, not from the board's name
(20260925-1300-board-facts-from-the-boot-policy):

- `boot.json` gains a required `board` section -- `boot`, `kernel`, `partitions`, `firmware` and,
  on a FIT board, `records` -- read by `mica-runkit` and `mica-deploy` and validated before use
  (`crates/mica-deploy/src/board.rs`). `BootKind::for_board`, the `FitLayout` enum, the firmware
  write-range tables and the deployment board table are gone; no board name is left in any crate's
  source. A board `mica-build` declares as data needs no `mica-core` change.
- **The partition numbers are the policy's too.** The device assumed boot 1, SYSTEM 2, DATA 3 in
  three places, which the plan's table did not list; `mica-build`'s layout rules allow a vendor
  partition, and such a disk would have been refused at first boot. `partitions` carries them, and
  `records` loses its `partition` key.
- **Disk contents still never choose writable offsets.** `FitLayout` is valid by construction
  (65536-byte copies, sector aligned, inside the partition, apart), and `boot_partition` holds the
  disk's boot partition to the policy's number, start and length before anything is written.
- A deployment and a firmware receipt are read in two steps: their shape (`parse_deployment`,
  `parse_firmware`), then the device (`components::admit`, `firmware::admit_firmware`). The parser
  gains `unsupported architecture` and `unsupported boot format`; `board/architecture mismatch` and
  `wrong boot format` are the device's. `wrong-board`, `wrong-arch` and `wrong-boot-format` now
  fire the parser's rule first, measured and recorded in `cases.json`.
- **Contract fixtures:** `cases.json` replaces the `boards` vocabulary with `boardPolicies`, each
  concrete board's section. `mica-build` pins the release that carries this and, in one commit,
  writes `board` into `boot.json`, follows `boardPolicies` in `deploy-pool --check` and deletes
  `src/image/device-fit-geometry.ts`. Until then its kernels carry no `board` and this device code
  would refuse them, which is why the pin and the section move together.
- **The rest of the board is declared as well**, in the same unreleased schema: `board.watchdog`
  names the watchdog by its driver identity (absent: `watchdog0`), micad's storage observer finds
  its tiers through the policy and reports the disk's own partition names, and firmware targets
  are named by mechanism -- `disk-range` and `emmc-boot` replace `rockchip-loader` and
  `amlogic-boot0`, which are refused rather than aliased, and `efi` takes a declared path. A board
  that uses an existing boot backend, firmware mechanism and architecture needs no code here.
- Versions: `mica-deploy` and `mica-lifecycle` are already an unreleased `0.1.0-5`. `micad` is
  `0.1.0-3` (its `0.1.0-2` is released, `20260921-0726`), so `mica-apid`, `mica-mqttd` and
  `mica-mqtt-broker` pin `micad (= 0.1.0-3)` and move with it: `mica-apid` `0.1.0-4`, the mqtt
  producer `0.1.0-3`.

## 2026-09-25 13:00 [plan]

- **Board facts from the signed boot policy** (plan and task
  `20260925-1300-board-facts-from-the-boot-policy`, approved, unclaimed): the FIT record geometry,
  the boot backend, the firmware target and the kernel format leave the board-name tables of
  `mica-deploy` for a required `board` section of `boot.json`, so a board `mica-build` adds as data
  needs no `mica-core` change. `mica-build` pins the release and writes the section in one commit.

## 2026-09-21 13:35 [feature]

- **An object is assembled from the chunks the device already holds**
  (`bccab41`). `fetch` asks for `<object url>.index` before it transfers an
  object whole, cuts its seeds -- the installed deployments' objects and the
  acquisition store -- once per fetch, and pulls only what it is missing from
  `<origin>/v1/chunks/<digest>`. The measurement that decided this: over the
  published `uefi-x64-prod` roots `20260916-0845` -> `20260920-0622`, with
  `mica-core`, `mica-podman`, `mica-system-base` and `mica-boards` all moved,
  **2.11 MB of the 65.3 MB image is content the device does not already have**.
  Nothing signed changed, because a delta is a transport for an object whose
  digest is already signed: the index and the chunks are unsigned, the
  assembled file goes through the same verification a whole download does, and
  any failure falls back to the whole object. `crates/mica-deploy/src/chunks.rs`,
  `tests/component-contracts/chunker.json`, `mica-deploy` and `mica-lifecycle`
  at `0.1.0-5`.
- **The update protocol an origin must serve is written down**
  (`89c5a7c`, `bccab41`). `docs/plan/20260921-1245-delta-transfer-and-the-update-protocol.md`
  Part 1 states the transport, the envelope wire order, catalog v2, selection,
  deployment v2, the objects and `MICAUPD1` as the device enforces them -- as a
  reader's guide to `tests/component-contracts/`, which stays the contract. It
  exists because the question "should mica-core be an independent upgrade unit"
  was answered no: splitting it out saves at most a fifth of the transfer on
  core-only releases, for a schema break.

## 2026-09-20 14:44 [progress]

Three commits of 2026-09-20 afternoon had no entry here, and the
`2026-09-15 01:15` entry below still describes `tests/vectors/`, which is gone.
That entry stays as written -- it was true then -- and this one says what moved.

- **The release-lock vectors are read, not copied** (`a55b5eb`).
  `scripts/gate/vectors.pin` names the mica commit and
  `scripts/gate/vectors-source.sh` fetches it into the git-ignored `repos/`
  cache, verifying `HEAD` is that commit; `tests/vectors/` is deleted. So
  `make locks-test` no longer runs a copy: it runs the specification's vectors
  at a pinned commit, reports the ones outside this repository's floor with the
  reason, and checks both directions of the comparison. The deleted copy was a
  snapshot of mica at `4df34ee` with one blob hand-edited.
- **`data` and `vectors-pin` are implemented, `board` and `apt` removed**
  (`a55b5eb`). No lock here carries a `board` or `apt` row, and a stale
  implementation answered `column-count` -- a claim about a row's shape --
  where `kind-unknown` is the honest answer.
- **Every refusal the reader can produce is accounted for** (`539b50a`).
  `tests/contract_rule_coverage.rs` reads the rule list out of
  `components.rs` at test time and requires each to be triggered there or named
  with where it is exercised. Two are unreachable by any input and are recorded
  as findings rather than counted as covered.
- **`mica-deploy` and `mica-lifecycle` are `0.1.0-4`** and the release gate's
  three version literals are derived from `producer.env`. The literals
  (`0.1.0-1`, `0.1.0-2`) were written when micad carried them; the bump to
  `0.1.0-2` in this set turned one case into a no-op and one into a false
  failure, which is how CI on `main` went red. The bump itself is the comment
  below: a Rust comment is in a file that builds the binary, so it moves the
  inputs hash even though the archive rebuilds byte-identically.
- **The project-quota chain is written where it is assumed** (`ed2cc93`).
  `mica-runkit` applies `prjquota` when it mounts DATA as PID 1;
  `mica-system-base`'s `mica-data-layout.service` assigns the project ids and
  sets the limits. Nothing here creates the filesystem.

## 2026-09-20 10:40 [change]

The console wears the brand mark (20260920-1035-console-brand-mark):

- The header, the navigation sheet and the sign-in plate showed a lowercase
  `m` in a tinted tile -- a stand-in from before the brand assets were in the
  image. They show `favicon.svg` now, the icon micaos.dev serves, which
  20260920-0811 already carried into the bundle.
- One asset, not two spellings of it: `BrandMark` is an `<img>` against
  `${import.meta.env.BASE_URL}favicon.svg` rather than a copy of the paths
  inlined in a component, so the brand's colours stay in the file a component
  may not name them in. The `logo-mark` rule and the `--logo` tokens are gone
  with the glyph they styled.
- The document's icon links are base-absolute (`/favicon.svg`, which Vite emits
  as `/_ui/favicon.svg`). They were relative, so a reload on a deep route asked
  for the icon under that route.

## 2026-09-20 10:30 [progress]

The shared fixtures say more about themselves:

- `envelope.json` and `catalog.json` carry `envelopeWireOrder`: the stored
  object is sorted for readability and the wire form is `schema`, `keyId`,
  `payload`, `signature`, which a reader requires and a re-serialised `Value`
  breaks. This repository knew and had not said it in the bytes.
- Every one of the 33 `invalid` cases in `cases.json` now names the rule it
  fires, measured and asserted, and the three whose mutation more than one rule
  would refuse are marked `alsoRefusedBy`. A refused-vector that could be
  refused by two rules tests neither.

## 2026-09-20 10:20 [docs]

The capability index, and the drift the inventory found
(20260920-0542-feature-inventory):

- `docs/features.md` answers the question neither `architecture.md` nor the
  design pages do -- *what can this device do, and where is that implemented*
  -- as one row per capability pointing at the owning crate and the section
  that explains it. No design prose is duplicated into it: a row is a pointer,
  so there is nothing in it to fall out of date except a path.
- `docs/design/deployment.md` named the retired products `x64-dev` and
  `x64-minimal`; it names the seven the assembly builds now. The same section
  gains `catalog.json`, the `schemas` block and the canonical field order of an
  envelope -- a wire-contract rule that 20260920-0100 recorded and no document
  stated.
- `docs/design/micad.md` had `4.4` and `4.5` sitting before `4.1`, from the
  order they were appended in; they follow `4.3` now.
- The system-log question from the device test is recorded as
  20260920-1010-system-log-view rather than answered by guessing: a bounded
  read over a unit allowlist and a live follow are different surfaces, and the
  follow would be the first long-lived connection apid serves.

## 2026-09-20 10:05 [feature]

The automatic check can be a time of day (20260920-0629-daily-check-time):

- `updates.json` gains `checkAt`, `HH:MM` **UTC** -- the rule
  `maintenance.windows` already follows, because the device clock is UTC and
  `time.timezone` is presentation only. Absent or `null` leaves the check on
  `checkIntervalMinutes` measured from the daemon's start, which is what every
  device does today.
- The driver checks once per crossing of that clock face, and the interval is
  not consulted while an anchor is set: an operator who named a time asked for
  that time, not for that time or sooner. `checkIntervalMinutes = 0` still
  means *no automatic checks* -- one switch turns them off, not two.
- **An anchor crossed while the device was down does not fire at boot.** The
  floor is seeded with the driver's start, so a device rebooting hourly under a
  daily anchor checks daily; the cost is that a device that is off at its
  anchor waits for the next one, and the interval remains the answer for a
  device that is not on at a predictable hour.
- **An unbelieved clock falls back to the interval**, rather than refusing: a
  time of day is the one thing here that needs a wall clock, and a clockless
  device must keep discovering updates. It is the same rule the install gate is
  the other half of -- that one refuses, because an install is time-keyed
  through the maintenance window.
- `Cadence` answers a wall time beside its monotonic one, read only after
  `ClockTrust::believed`, so an interval still cannot move when the clock is
  set. `POST /api/v1/update/config` accepts `checkAt`, `GET /api/v1/update`
  reports it beside the resolved cadence, and the console's policy pane has the
  field with its UTC label.

## 2026-09-20 09:24 [feature]

Bluetooth is a declared trust list over BlueZ, and pairing is an action with an
operator in it (20260920-0813-bluetooth-pairing, plan 20260920-0812):

- `bluetooth.json` carries the subtree: the switch, `discoverable`, the
  advertised `alias`, the legacy `pin` and the paired devices by address, each
  with the name it gave, `trusted` and `blocked`. Declared and observed are
  named apart -- a paired phone out of range and a phone in range nobody paired
  are different facts, and the console shows which side each row came from.
- The reconciler owns the unit, the adapter properties and the trust flags. It
  is the first reconciler whose subject is **optional hardware**: a board with
  no radio reports `unsupported` with the reason rather than failing, because a
  reconciler that fails there fails on every pass forever. An absent alias
  advertises the hostname. Only *paired* undeclared devices are removed --
  sweeping merely-seen ones would delete the scan an operator is reading.
- `GetBluetooth` reads the adapter and every device BlueZ knows over the system
  bus, with the observer's usual unavailable default; `SetBluetoothDiscovery`,
  `PairBluetoothDevice`, `ConfirmBluetoothPairing` and `RemoveBluetoothDevice`
  are the actions. micad registers an `org.bluez.Agent1` of its own: one
  pending request at a time, a bounded wait for the confirmation, and the
  device list written under the same apply lock every settings write takes, so
  a pair landing during a reconcile is not lost.
- **The PIN is the device's own, shown rather than hidden.** A legacy peer with
  no display is answered with `bluetooth.pin`, which is editable and defaults
  to a value derived from this device's identity -- the access point key's
  rule, applied to the one value here somebody has to read off the screen and
  type on the other device. Not a fleet-wide constant: a constant would pair
  every device in a fleet with one number.
- `GET/PUT /api/v1/bluetooth`, `POST /api/v1/bluetooth/discovery`, and the
  device routes (`pair`, `confirm`, `DELETE`) -- a resource group, because the
  settings write route's allowlist does not carry this subtree. The console's
  network page gains a Bluetooth tab: the adapter, the scan, the passkey
  confirmation and the paired list.
- `application-data` reset clears `bluetooth.devices`: a paired phone is
  operator data, the conclusion the container work reached for its units.
- No version bump: `micad` is already `0.1.0-2` and `mica-apid` `0.1.0-3` in
  this unreleased set, and one bump covers every change since the release the
  guard measures against. What the earlier `micad` bump missed is fixed here --
  `mica-apid`, `mica-mqttd` and `mica-mqtt-broker` pin `micad (= 0.1.0-2)`
  now, as `scripts/deb/producers.sh` requires, and `mica-mqttd` and
  `mica-mqtt-broker` are `0.1.0-2` because that pin sits in their producer
  directory and moves their inputs hash.

## 2026-09-20 08:11 [change]

The console chrome follows micaos.dev, and the Wi-Fi tab gains its missing
halves (20260920-0811-console-chrome-and-wifi):

- The language and appearance pickers are icon-triggered selects, the shape the
  site uses for the same two controls. The language picker was a combobox with
  a text input, which in a header slot read as an empty search box rather than
  as the language the console is in; the theme control was a segmented control
  inside the settings menu. Both are in the header now and the menu is down to
  signing out.
- The console has an icon: the site's `favicon.svg`, `favicon-32.png` and
  `apple-touch-icon.png`, carried in the image because a device may have no
  route off it. The SVG is the site's with its c2pa manifest stripped -- 8 KB
  of provenance metadata about an icon, which is 95% of that file.
- `lo0` and `dummy*` join the loopback and the container engine's `veth` ends
  in what the interface list hides. A name an integrator chose, like
  `lolink0`, still is not.
- **Wi-Fi scanning**: `POST /api/v1/wifi/client/scan` and micad's `ScanWifi`
  run `SCAN` then `SCAN_RESULTS` over wpa_supplicant's control socket, bounded
  at 64 results. POST, because a scan sweeps every channel and briefly costs
  the station its link. A hidden network is reported with an empty name rather
  than dropped.
- **The access point is configurable**: `GET/PUT /api/v1/wifi/ap`, with `psk`
  read as `<redacted>` and kept when a write omits it -- the known-network
  rule, so changing a channel cannot publish an open access point.
- The console's Wi-Fi tab shows the access point, the networks on the air and
  a Connect that opens the add dialog with the name filled in: joining a
  network is declaring it.

## 2026-09-20 07:46 [feature]

An update archive can be uploaded from the console
(20260920-0746-update-upload):

- `ImportUpdate(path)` stages an offline `MICAUPD1` archive the way a fetch
  stages a download -- same probe, same busy slot, same read bound, same
  `mica-deploy` code path, so the signature, the product and every object
  digest are checked exactly as they are for a download. **Not gated on the
  network policy**: a metered link and a mode of `off` have nothing to say
  about a file an operator carried here.
- The path is bounded to `/mica/updates/uploads`, which is what keeps the bus
  member from being a way to hand `mica-deploy` an arbitrary file, and the
  upload is removed once the import settles either way.
- `POST /api/v1/update/import` streams the body there under a name apid draws,
  refuses a body that does not begin with `MICAUPD1` after eight bytes rather
  than after a gigabyte, and answers 202.

## 2026-09-20 07:37 [feature]

The access point says who is connected to it (20260920-0737-ap-stations):

- The AP reconciler renders `ctrl_interface=/run/hostapd`. Without it hostapd
  opened no control socket, so the question had nowhere to be asked.
- The observation gains `accessPoint`: per interface, each station's address,
  signal, connected time and byte counters, walked over that socket with the
  client wpa_supplicant is already asked through and bounded at 64 stations.
  **No credential**: hostapd's reply carries key negotiation state and none of
  it is read.
- An interface with no hostapd on it is reported as one rather than as an
  access point with no clients, and the console shows the stations beside the
  associations -- one is this device joining a network, the other is a network
  joining this device.

## 2026-09-20 07:29 [feature]

Containers are declared, rendered by Quadlet, observed through podman
(20260920-0640-container-management, plan 20260920-0639):

- `container.units` holds them: image, command, environment, published ports,
  volumes, restart policy and `autoStart`, skipped when empty so a device that
  declares none writes the document it wrote before. A volume's host path must
  be under `/mica/`, checked in `micad-settings` and not only in apid, because
  the document is writable without apid.
- The reconciler renders `50-mica-<name>.container` per entry, compares before
  writing, sweeps its own files and only its own, reloads so Quadlet
  regenerates, and starts each unit its entry asks to start. Nothing is
  rendered with the switch off: the directory is not mounted then.
- `containers.rs` reads the engine and never writes it -- `podman ps --all` and
  `podman images`, bounded in time and output, no `run`, no `rm`, no `pull`.
  Starting a unit pulls the image if it has to, which keeps the one writer of
  container state the unit file.
- `GetContainers` names the declared map and the engine read apart rather than
  merging them; `StartContainer`, `StopContainer` and `RestartContainer` are
  systemd verbs on the generated unit, refused for a name the settings tree
  does not hold.
- apid serves `GET /api/v1/containers`, `PUT/DELETE /api/v1/containers/{name}`
  and `POST /api/v1/containers/{name}/{action}` for exactly three actions, and
  the console's containers page lists declared beside observed with start,
  stop, restart, edit and a removal that says data under `/mica/` stays.
- The `application-data` reset clears the declarations with the data
  (20260920-0730-reset-clears-containers). A declared container is an operator
  application: clearing the volumes and keeping the map would leave a reset
  device running the same workload against an empty volume.

## 2026-09-20 07:23 [feature]

Static routes and a DHCP server on a declared interface
(20260920-0723-routes-and-dhcp-server):

- `IfaceSettings` gains `routes` and `dhcpServer`, both skipped when empty, so
  an entry that declares neither serializes exactly as it did before they
  existed. `network.json` keeps schema version 1 for that reason: bumping it
  would make every older micad refuse every newer document, including the ones
  that use nothing new.
- Rendered as networkd `[Route]` sections and a `[DHCPServer]` block with
  `DHCPServer=yes`. **A bridge port renders neither**: a port's addressing is
  the bridge's, so a route or a server on one is a configuration networkd would
  accept and nothing could use.
- apid refuses, before anything is written: a destination that is not a
  network, a next hop that is not an address, a second default route beside
  `static.gateway`, a server on a link that is itself a DHCP client, an empty
  pool, and an announced DNS server that is not an address.

## 2026-09-20 07:00 [change]

The network page shows what the device has and edits it
(20260920-0700-network-page):

- The interface list hides the loopback and the container engine's `veth`
  ends, always keeping a declared entry. Observation moved: the device-wide
  half (routes, DNS, radios) to its own tab, the per-interface half to that
  interface's page, rendered by the same component.
- An interface can be edited and removed: kind-level fields (VLAN parent and
  id as selectors over the links the device actually has, bridge ports the
  same, tunnel listen port) and a delete behind a confirmation. A tunnel is
  offered neither DHCP nor a gateway, and the page says why.
- The WireGuard tab prints the device's own `[Peer]` block with a copy
  control, read from the public key the network reconciler publishes.
- Two new routes, because the console could not do this without them:
  `GET/PUT /api/v1/wifi/client` for the station's radio and switch -- the
  interface was on no writable path at all -- and
  `PUT /api/v1/wifi/client/networks/{ssid}`, where **`psk` absent keeps the
  stored key**: a read substitutes the redaction sentinel, so an operator
  changing a priority has not been shown the key.

## 2026-09-20 06:58 [feature]

`GET/PUT /api/v1/mqtt`, one resource for the broker and the bridge:

- The listener is one decision -- an address and a port that take effect
  together -- so it is one document and one write, not three entries on the
  scalar settings allowlist. `GET` answers `{ configured, observed }` without
  merging them: the declared listener and the one the broker is bound to differ
  for as long as a reconcile takes, and after a failed one until someone looks.
  `PUT` replaces the subtree, answers the apply task's id, and refuses a bind
  address that is not an IP literal or a port of 0 -- failures the settings tree
  would otherwise pass to a broker that then will not start.
- `mqtt.enabled` stays a scalar write: the service switch owns it, and the
  listener form carries it through unchanged so editing a listener cannot start
  or stop the broker.
- The console's MQTT service page gains the listener form and an observed panel
  (both units, the bound listener, the rendered configuration path), and the
  open-listener warning returns: bound off loopback, authentication off
  (20260920-0733-mqtt-resource).
- `mica-apid` is `0.1.0-3`: every change in this entry and the one below is in
  that crate or its UI, so its inputs hash moves and the guard requires the
  bump. `micad`, `mica-mqttd`, `mica-mqtt-broker`, `mica-sftp-server`,
  `mica-deploy` and `mica-lifecycle` are untouched.

## 2026-09-20 06:55 [change]

Console work from a device test, and one credential that should never have
existed:

- **First-run setup mints no API token.** `POST /api/v1/setup` returned a
  bearer secret on every call because it was once one of two setup clients; the
  server-rendered wizard it was written for is gone, so the console was the only
  caller and every operator got a long-lived credential they never asked for.
  `SetupToken` is now `SetupResult`, carrying the session's CSRF token alone,
  and `POST /api/v1/tokens` is the one way to get a bearer credential. The route
  also documented "Answers 200" while returning 201
  (20260920-0655-setup-mints-no-token).
- **The overview names the uplink.** Its network card took the first observed
  interface carrying any address, which is the loopback on every device, so it
  read `lo · 127.0.0.1`. It now reads `/api/v1/network/status` and selects the
  interface a default route leaves by, falling back to the first globally
  addressed non-loopback link. The page also gained a device panel -- machine
  id, board, software version, deployment, kernel -- from the
  `/api/v1/system/info` document it already fetched
  (20260920-0549-overview-uplink-and-identity).
- **The web terminal is removed**, not deferred: card, detail pane, switch,
  window, simulation flag, colour tokens and e2e block. It showed a fixed
  transcript and took no input, and a root shell over HTTPS is not being built.
  `ServiceDefinition` now requires a settings path and a state path, so every
  card in the catalogue is one the device can answer for
  (20260920-0611-remove-web-terminal).
- **The update cadence reads in days and hours.** `checkIntervalMinutes` is
  unchanged on the device; the field is a number and a unit, shown in the
  largest unit that divides the stored value exactly, so a daily check reads as
  `1 Days` and not `1440`. A check anchored to a time of day is
  20260920-0629-daily-check-time and is not started: micad schedules from an
  interval, not an anchor (20260920-0628-update-cadence-unit).

## 2026-09-20 01:00 [progress]

The update protocol becomes bytes both sides verify:

- `crates/mica-deploy/tests/component-contracts/catalog.json` is a fifth shared
  fixture from the same test-only generator: one signed `mica/catalog/v2`
  document serving the golden deployment from a real source URL, with its
  objects at `<origin>/v1/objects/<sha256>`. `tests/contract_protocol.rs`
  verifies it through `verify_catalog`.
- `cases.json` gains a `schemas` block, accepted and refused strings, driven
  through all three readers. `mica/catalog/v1`, `mica/deployment/v1` and
  `mica/rootfs/v1` are named as refused because a second update server
  (`micaoss/mica-fleet` `apps/updates`) implements them today; the reader takes
  v2 only, with no aliases.
- Building the vector caught that field order is part of the wire contract: an
  envelope re-serialised from a `Value` sorts its four fields alphabetically and
  is refused as `noncanonical envelope`. No prose said so.
- `mica-deploy` and `mica-lifecycle` are `0.1.0-3`. The fixtures are test files
  and excluded, but the regeneration **example** beside them is not: a
  `crates/*/examples/**` file sits in the crate tree and moved both inputs
  hashes. `scripts/deb/inputs.sh` now excludes examples -- a producer compiles
  its binaries with `cargo build --bin`, which never builds one -- and
  narrowing the manifest took the same one bump.

## 2026-09-19 10:30 [fix]

The device side follows the 2026-09-16 board rename (`x64` to `uefi-x64`,
`virt-arm64` to `uefi-arm64`; `cx3576` and `s905x5m` unchanged):

- Every board match in `mica-deploy` named the retired spellings, and the
  assembly signs the new ones -- the published
  `mica-uefi-x64-dev-20260919-2103.micaupd` envelope decodes to
  `"board":"uefi-x64"`. `BootKind::for_board` runs in `mica-runkit` as PID 1,
  so such a device does not boot, rather than merely failing to update.
- New names only, no aliases: `boot.rs`, `components.rs` and `firmware.rs`
  (board-to-architecture and the EFI file name), the six test files and the
  three shared fixtures. All four sites fail closed, so a device refuses and
  never mis-targets.
- `cases.json` now carries the board vocabulary as data, with the retired names
  listed as refused, and `board_vocabulary` drives it through both readers. The
  shared fixtures were regenerated: `deploymentId` is
  `7091da552d8cd8359568dc7803c0e78d46626597c15f665b85c5f0c4818409cf`.
- `mica-deploy` and `mica-lifecycle` are `0.1.0-2`: both are built from the
  `mica-deploy` crate, so the rename moved their inputs hashes (deploy
  `acd29506…` to `12b8966d…`, lifecycle `2ddf75e8…` to `0061a4d4…`, measured
  against the pool of `20260916-0916`). `micad`, `mica-apid`, `mica-mqttd`,
  `mica-mqtt-broker` and `mica-sftp-server` are unchanged, and their inputs
  hashes are bit-for-bit what that release published.

## 2026-09-16 09:12 [progress]

The build env moves to mica-build-env `20260916-0735`:

- `locks/mica-build-env.lock` is that release's asset unchanged and
  `locks/pins/mica-build-env.pin` records it with the trust hash
  `7df0af68761a63c6517b37a739a57ce947da53fbe558aba2646368e53724bf0a`. The
  images mica-core reads are base and rust; the new `bsp` image is for the
  board repositories and is never read here.
- The packages rebuild byte-identically under the new images: on amd64 all six
  at `0.1.0-1` reproduce the bytes released as `20260915-1135`. On arm64 they do
  not, but the cause is not the images: the same station with the old
  `20260915-0138` lock produces exactly the same arm64 bytes, because
  `docker buildx build --platform linux/arm64` on an amd64 host is QEMU
  emulation while the released archives were built natively. The record
  `docs/task/20260916-0912-emulated-arm64-bytes.md` holds what is still unknown.

## 2026-09-16 08:12 [progress]

`mica-apid` is `0.1.0-2`, and the inputs hash stops counting check-only files:

- The pipefail fix in `crates/mica-apid/ui/verify-ui-policy.sh` changed a file
  inside the apid crate tree, so the inputs guard refused the package at its
  released version. The script builds nothing the package carries, so it, the
  UI's `run.sh`, `eslint.config.js` and `playwright.config.ts` are now excluded
  from `scripts/deb/inputs.sh` alongside the test files -- repository
  housekeeping must not look like a package that changed
  (`mica:docs/decisions/2026-09-15-package-versions.md`, Rationale).
- Narrowing the manifest changes the hash too, so `pkgs/apid/producer.env`
  declares `VERSION="0.1.0-2"` with its epoch unchanged; the archive's bytes are
  the same as `0.1.0-1` apart from the version. Every other producer stays at
  `0.1.0-1`, and `micad (= 0.1.0-1)` is unchanged in its dependents.

## 2026-09-15 10:59 [progress]

Packages are locked by their declared version (task `20260915-1059-package-versions`,
user decision 2026-09-15, R0-R8):

- Every producer declares `VERSION="0.1.0-1"` and `SOURCE_DATE_EPOCH` in its
  `producer.env`; the `VERSION` file and `scripts/deb/version.sh` are gone. No
  version or control field carries a commit, date or release, and
  `Mica-Source-Commit` is removed.
- `micad --version` and `mica-apid --version` print the package version; micad's
  system information reports it as the daemon version and no longer reports a
  commit, a git stamp or a commit date. The diagnostic snapshot schema is 6 and
  its redaction schema 8.
- `scripts/deb/inputs.sh` hashes each producer's inputs per architecture; a
  release records it as `mica.inputs` on each pool layer, and pool manifests
  carry only `mica.source-repo` and `mica.arch`, so an unchanged pool keeps its
  digest.
- `scripts/build/reuse.sh` checks the packages against the newest release under
  the rules, in CI and before a release: a package at its released version must
  keep its inputs and bytes, and a lower version is refused.
- The rootfs component is `mica/rootfs/v2`, without a `version`: like the kernel
  component, its id changes only with its content, so a root rebuilt from the
  same inputs keeps its id. The release version stays in `mica/deployment/v2`.
  The contract fixtures are regenerated. The root carries no
  `/usr/share/mica/release-identity.env` any more, and micad reads none.

## 2026-09-15 07:02 [progress]

Update packages (task `20260915-0657-update-packages`, user decision
2026-09-15):

- A deployment descriptor is `mica/deployment/v2` with a signed `product`; the
  device reads its own from `PRODUCT=` in `/usr/lib/mica/product.conf`, and
  `check`, `fetch`, `import` and `install` refuse another product.
- The catalog is `mica/catalog/v2`: channel heads, generation uniqueness and
  selection are keyed by board, product and channel.
- `import` accepts a MICAUPD1 archive carrying a subset of the descriptor's
  objects (root-only or kernel-only updates) when the rest is already present.
- The contract cases gain the product file, product cases and archive cases;
  the fixtures are regenerated with product `x64-dev`.

## 2026-09-15 01:15 [progress]

Release lock migration, stage 3 (`mica:docs/design/release-lock.md`):

- Inputs: `locks/mica-build-env.lock` (mica-build-env `20260915-0138`, unchanged)
  with `locks/pins/mica-build-env.pin`, and `locks/upstream.lock`, which pins
  nothing of this repository's own. `build-env-image.lock`, `build-env-release`,
  `scripts/build/build-env.sh` and its test are gone.
  `scripts/build/check-lock.sh` applies the lock, upstream and pin rules,
  `scripts/build/locks.sh check|verify` reads `locks/`, and `from.sh` resolves
  `rust`, `base` and the upstream images by digest. The buildkit image of the
  container builders and the Dockerfile frontend come from the lock's
  `upstream` rows.
- Release: the pools `ghcr.io/micaoss/mica-core:pool.<arch>.<release>`, one
  manifest per architecture, and a GitHub Release carrying exactly
  `mica-core.lock` (release, pool and package rows) and `SHA256SUMS`. No `.deb`
  assets and no `core-pkgs.lock`.
- `make locks-test` runs the specification's vectors (`tests/vectors/`) and the
  reader tests; `make release-test` checks the written lock.

## 2026-09-14 20:46 [progress]

`make offline` (`scripts/build/offline.sh`) builds both pools from a clean
checkout with only the pinned inputs and prints where they are:
`_out/debs/<arch>/pool/`, `Packages`, `SHA256SUMS` and `manifest.txt`, the same
outputs `make pool` writes. It refuses a dirty tree and checks the build-env
record offline.

## 2026-09-14 19:40 [progress]

A release also publishes its archives as the OCI artifact
`ghcr.io/micaoss/mica-core:pool.<tag>` (one manifest, one layer per archive of
both architectures) and attaches `core-pkgs.lock`, one row per archive with its
version, sha256 and the pool by digest; `SHA256SUMS` covers the lock. The pushed
tag is never re-pointed and everything is read back with no credential. The
publisher test covers the push, a rerun, a re-pointed tag, a private package
and a refused token against a fake registry. Takes effect from the next release.

## 2026-09-14 11:50 [progress]

The build environment moves to `micaoss/mica-build-env` release
`20260914-1129`: its `build-env-image.lock` (sha256 `2ef37b0cf243…`) replaces
the previous one whole and `build-env-release` records the tag and its
`SHA256SUMS` hash `6c582b2a6ff7…`. `make check`, the amd64 pool and the
package gate with its rebuild pass on the new images.

## 2026-09-14 11:29 [progress]

The signed update contract drops the former project name (user decision): the
readers accept only `mica/deployment/v1`, `mica/kernel/v1`, `mica/rootfs/v1`,
`mica/update-envelope/v1` and `mica/firmware/v1`, and offline archives
(`.micaupd`) only with the magic `MICAUPD1`; nothing else is accepted. The
contract fixtures in `crates/mica-deploy/tests/component-contracts/` are
regenerated by `examples/component-contract-fixtures.rs` with TEST-ONLY keys
derived from labelled seeds, with the x64 kernel release `6.12.107`;
`tests/contract_fixtures.rs` holds the committed bytes to the generator. No
name from before the Mica OS rename remains in the tree.

## 2026-09-14 10:49 [progress]

The remaining names from before the Mica OS rename are mica: the UI package
name in `bun.lock`, the UI's backup file extension `.micabak` and its uptime
caption key, and test fixture names. The one exception is the signed update
contract shared with mica-build (its six schema ids, the offline archive
magic and extension, and the contract fixtures), which is a published
interface and moves together with mica-build.

## 2026-09-14 06:20 [progress]

CI and release builds use caches (`actions/cache` v6): third-party cargo
artifacts, the cargo registry and the bun package cache, keyed by the build-env
lock, the Cargo manifests and lock and `bun.lock`. Only pushes to `main` save,
after `scripts/build/cache-prune.sh` strips the workspace crates' own
artifacts; pull requests and releases only restore. No gate is skipped; a
package built from the pruned cache was checked byte-identical to one built
from scratch (micad, mica-sftp-server).

## 2026-09-14 05:07 [progress]

The task and plan records are cleared: every record described work or proposals
from before the repository was reset (the dropbear/SFTP work, the workspace
convergence, the build-env v0.0.1 move, the independent apid upgrade and the
per-producer rebuild proposals). The unwired acceptance script of the
independent-apid-upgrade proposal, `scripts/gate/interface-dependency-test.sh`,
goes with it.

## 2026-09-14 04:51 [progress]

The image profile is gone from micad: `/usr/lib/mica/profile.conf` is no
longer read (no package ships it since mica-system-base dropped
`mica-profile-dev` and `mica-profile-prod`), and `micad` no longer depends on
`mica-profile`. First-boot provisioning seeds `access.ssh.enabled = false` as
before. Development and production images are to differ through the kernel
command line; micad reads nothing for that yet.

## 2026-09-14 04:13 [progress]

Simplification: comments and old compatibility baggage trimmed.

- The crate that builds `micad` is `crates/micad` (package, library and
  executable `micad`); `mica-core` names only the repository.
- The packaging layer is reduced to what this repository uses.
  `producer.env` has three keys (`PACKAGES`, `ENABLEMENT`, `CONTEXTS`);
  every producer builds amd64 and arm64 with a `prepare.sh`; the package gate
  keeps every check (package set, architecture, one stamp, no overlap or
  `Replaces`, exact local pins, copyright, enablement, no conffiles, POSIX
  maintainer scripts, byte-identical rebuild) without the import-lock,
  virtual-package, `all`-architecture and instance machinery. Removed: the OCI
  registry and lock scripts, the pre-flight, `scripts/deb/README.md`,
  `scripts/build/build-target.sh` and `build-aarch64.sh`, `deps/`.
- Comments across the Rust crates, the UI, the scripts, the units and the
  D-Bus policy are cut to what explains the code: internal plan, milestone and
  section references, pointers to documents outside this repository and
  historical narrative are gone. The published OpenAPI descriptions keep their
  content and lose only those references. Package descriptions are short and
  user-facing.

## 2026-09-14 04:10 [progress]

Architecture and development documentation for readers outside the project:
`docs/architecture.md` (components, the management-plane model, access,
updates and boot, security boundaries, source map), `docs/development.md`
(prerequisites, building, the cargo loop in the build image, running the
daemons locally, the UI, common changes, releasing) and component designs in
`docs/design/` (micad, apid, MQTT, deployments and boot, packaging and
release). `pkgs/README.md` names the current make targets and paths.

## 2026-09-14 03:30 [progress]

mica-core moves to the `micaoss` organisation with a fresh history, following
mica-podman. `ci.yml` runs the gates and builds, packs and gates every package
for amd64 and arm64 on push and pull request (a docs- or markdown-only change
builds nothing) and publishes nothing; `release.yml` runs only when a release
`<YYYYMMDD-HHMM>` is published, builds that tag and attaches every archive and
`SHA256SUMS` to it (`scripts/build/release.sh`, read back anonymously, tested
by `make release-test`). The shared build (`build.yml`) compiles and packs
arm64 on arm64 runners with no emulation (`scripts/build/build-deb.sh`
compiles in the host's image); `scripts/deb/package-gate.sh` gates one
architecture (`--arch`) or both pools without rebuilding (`--no-reproduce`).
The OCI pool publication, the `v<VERSION>` tag release and the published-pool
build decision are gone. No file names the former organisation. Not yet
published.

## 2026-09-14 02:40 [progress]

`build-env-image.lock` is the lock of the mica-build-env release
`20260914-0128`, now published from `micaoss/mica-build-env`: the loader
downloads from `github.com/micaoss/mica-build-env` and accepts only
`ghcr.io/micaoss/mica-build-env` references, with no fallback to the old
organisation's namespace. Every image digest is new (Python moved from the C image
to the base image), so the full gates have not yet run in these images. Not
yet published.

## 2026-09-14 02:00 [progress]

The build-env images come from `build-env-image.lock` at the repository
root: the lock asset of the mica-build-env release `20260914-0042`,
committed unchanged, with `build-env-release` recording that tag and the
sha256 of its `SHA256SUMS`. `make deps` verifies the lock against that
release anonymously and `make deps-check` checks both files offline
(`scripts/build/build-env.sh`, tested by `make build-env-test`). The old
`deps/build-env.json` pin and the fetched `build-env/` directory are gone,
because the releases they named and their images no longer exist. All four
image digests are new, so every package producer builds in new image bytes
and the full gates have not yet run in them. The pool decision compares the
Rust and base images of the verified locks of both commits. Not yet
published.

## 2026-09-14 01:10 [progress]

CI no longer runs on a branch push or pull request. A release is manual, as
in bkhq/bkd: bump `VERSION` by a commit on main and push the tag
`v<VERSION>`, or dispatch `release.yml` with it. The workflow checks the tag
(`scripts/build/release.sh check`: `v<VERSION>` on main), runs every gate,
publishes the pool and then the GitHub release of the tag, whose notes name
both pools by manifest digest and every archive by sha256, read back with no
credential. Dispatched without a tag it is a dry run that publishes nothing.
A release whose commit changes no package input since the last published
pool is refused. Not yet published.

## 2026-09-14 00:55 [progress]

The package producers moved from `packaging/deb/<producer>/` to
`pkgs/<producer>/` (history kept), with the shared copyright file and the
packaging contract at `pkgs/copyright` and `pkgs/README.md`; each producer's
`family` build context is `pkgs`. Producer compile outputs moved from the
root `target-deb/` to `_out/target-deb/`. Package contents are unchanged.
Not yet published.

## 2026-09-14 00:40 [progress]

The mica-build-env pin is the new v0.0.1 (`SHA256SUMS` de740ff5798e), cut
after mica-build-env's history and release reset; the earlier v0.0.1 and
v0.0.2 and their image digests no longer exist. Its generated `images.env`
holds only the four `IMAGE_MICA_BUILD_*` references, which is all this
repository reads. The Rust and base images are new digests with the same
toolchain versions. Not yet published.

## 2026-09-13 23:55 [progress]

CI builds and publishes a pool only for a commit that needs one
(`scripts/build/pool-decision.sh`). When everything changed since the nearest
ancestor with a complete published pool (both architectures, this
repository, that commit) is docs, markdown, `.gitignore` or a release pin
whose verified releases pin `IMAGE_MICA_BUILD_RUST` and
`IMAGE_MICA_BUILD_BASE` to the same repository and digest (a tag may differ), the commit builds nothing and gets no pool; any other
change, and anything the step cannot establish, builds the full pool with
every existing gate. Not yet published.

## 2026-09-13 23:10 [progress]

The mica-build-env pin is v0.0.2 (`SHA256SUMS` b75932fc6df3). Its `RULES.md`
and all four `IMAGE_MICA_BUILD_*` digests equal v0.0.1's; it drops the
rust-check image this repository no longer used. Not yet published.

## 2026-09-13 22:40 [progress]

Built on the mica-build-env v0.0.1 release (`20260913-2200-build-env-release-v0.0.1`).
`deps/build-env.json` pins the version and the sha256 of its `SHA256SUMS`;
`make deps` (`scripts/build/build-env.sh`) downloads the release assets and
refuses them unless both hashes match. The Rust gate, the builds, the
boot/shutdown and IO fault suites run in the published `IMAGE_MICA_BUILD_RUST`,
the UI build and the packing in `IMAGE_MICA_BUILD_BASE`, both by digest; no
`LOCAL_MICA_BUILD_*` image is built or named. The scripts that run are this
repository's own copies of the release reference implementation
(`scripts/build/from.sh`, `scripts/deb/`); `tools/deps.sh` and the source pin
are gone, and CI no longer builds images. The independent mica-apid upgrade
is a proposal only (`docs/plan/20260913-2230-independent-apid-upgrade.md`).
Not yet published.

## 2026-09-13 21:20 [progress]

mica-apid is its own executable and producer; micad no longer carries apid.
Every name in this repository that predates the Mica OS rename is mica, with no compatibility: the
D-Bus object /com/mica/micad, MICAD_* variables, the mica account, the pool
subdirectory, boot entry, verity and U-Boot names, and the core-owned schema
ids. The signed update contract shared with mica-build (the deployment,
kernel, rootfs, update-catalog, update-envelope and firmware schema ids and the
archive magic) waits for mica-build's re-signed fixtures. Not yet
published.

## 2026-09-13 20:45 [progress]

The apid executable is `mica-apid` (`/usr/bin/mica-apid`, a link to `micad`;
`micad` keeps its name) and the system DATA namespace is mounted at `/mica`,
both on the user's request. Units, packages, D-Bus names
and API keys are unchanged. Not yet published.

## 2026-09-13 20:16 [progress]

One crate workspace (`20260913-1935-workspace-convergence`, phases 1 and 2):
every crate under `crates/`, the producers under `packaging/deb/`, the shell
entry points under `scripts/build/` and `scripts/gate/`. The Cargo packages
`micad` and `apid` are now `mica-core` and `mica-apid`; executables, Debian
packages, units, D-Bus names and installed paths are unchanged. mica-deploy
joined with its history (b698b10dd6aa, merged unchanged, then moved into
`crates/mica-deploy`, `crates/lifecycle-sys`, `packaging/deb/{deploy,lifecycle}`
and `scripts/gate/`), so the pool has seven packages; `mica-runkit` is built
alone on the static route. Not yet published.

## 2026-09-13 03:30 [progress]

Created from the daemon package directory of `micaoss/mica-build` (163 commits kept through
`git subtree split`, then the tree at the Mica OS rename). Moved in with the
workspace: the Rust gate driver (`gate/rust-gate.sh`), the built-in UI's
build contract test (`gate/apid-ui-build-contract-test.sh`) and the D-Bus
policy test (`tests/dbus-policy-test.sh`). The API harness that boots the
assembled image stays in the assembly (`mica-build:tests/apid-api/`); for
its build-time half the `mica-apid` archive now ships
`/usr/share/mica-apid/openapi.json`. The substrate is the `mica-build-env`
source pin at `build-env/`; the four packages are published as
`build-<commit12>` and imported by `mica-build` through `deps/packages/`
(Phase 5 of `20260911-2006-split-package-repositories`).
