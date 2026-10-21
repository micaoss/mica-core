export interface TaskAccepted {
  taskId: string
  task?: { id: string; status: string }
}

export interface TaskRecord {
  id: string
  operation: string
  /// `network.apply`, `app.apply` or `settings.apply`.
  kind: string
  dotPath: string
  source: string
  status: string
  /// `saved`, `applying`, then `applied`, whatever the outcome.
  phase: string
  /// The interfaces a finished network apply added, changed or removed.
  interfaces?: string[]
  enqueuedAt: string
  startedAt?: string
  finishedAt?: string
  outcome?: string
  message?: string
  foldedCount: number
}

export interface Health {
  apid: string
  micad: string
  checkedAt?: number
  detail?: string
}

export interface ObservedInterface {
  index?: number
  name?: string
  kind?: string
  type?: string
  driver?: string
  administrativeState?: string
  operationalState?: string
  carrierState?: string
  addressState?: string
  ipv4AddressState?: string
  ipv6AddressState?: string
  onlineState?: string
  mtu?: number
  hardwareAddress?: unknown
  addresses?: unknown[]
  dns?: unknown[]
  routes?: unknown[]
}

export interface BluetoothDevice {
  address: string
  name: string
  paired: boolean
  trusted: boolean
  blocked: boolean
  connected: boolean
  rssi?: number | null
}

/// `GET /api/v1/bluetooth`: the declared trust list, the adapter, the devices
/// BlueZ holds, the pairing window and whatever waits in it. Declared and
/// observed are named apart because they answer different questions.
export interface BluetoothOverview {
  enabled: boolean
  declared: Record<string, { name: string; trusted: boolean; blocked: boolean }>
  adapter: AvailableFact & {
    address?: string
    /// What it advertises: the hostname.
    name?: string
    powered?: boolean
    discoverable?: boolean
  }
  devices: AvailableFact & { entries?: BluetoothDevice[] }
  /// Pairings waiting for the console, each with the six digits to compare;
  /// `passkey` is null for a peer with no display.
  pending: { name: string; mac: string; passkey: string | null; since: number }[]
  /// The open pairing window, or null.
  pairWindow: { expiresAt: number } | null
}

/// What opened a BLE provisioning window.
export type BleTrigger = 'unclaimed' | 'boot' | 'offline' | 'manual'

/// `GET /api/v1/bluetooth/provisioning`.
export interface BleProvisioning {
  state: 'off' | 'waiting' | 'connected' | 'done'
  trigger: BleTrigger | null
  /// Seconds since the epoch; null while `offline` waits for the route.
  expiresAt: number | null
  /// The manual window's eight digits.
  code?: string
  autoWhenOffline: boolean
  /// Whether the administrator password works over Bluetooth.
  verifierReady: boolean
  lockedUntil: number | null
  /// A session holds Wi-Fi: its writes answer 423.
  wifiLocked: boolean
  /// Newest first.
  history: { at: number; trigger: BleTrigger; outcome: string }[]
}

/// The channels an access point may use in one country.
export interface WifiChannels {
  countryCode: string
  channels: number[]
  /// Whether channel 0, the access point's own choice, is offered.
  auto: boolean
}

export interface WifiAccessPoint {
  mode: 'off' | 'provisioning' | 'always'
  interface: string
  ssid?: string
  /// Reads as `<redacted>`; omit it on a write to keep the stored key.
  psk?: string
  channel: number
  countryCode: string
  address: string
  holdDownSeconds: number
  graceSeconds: number
  /// Absent is WPA2-PSK.
  security?: 'wpa2-psk' | 'wpa2-wpa3' | 'open'
  /// Read whenever the address is valid. Absent on a write hands out from
  /// the address after the access point's to the end of its subnet; an
  /// absent lease is the DHCP server's own default.
  dhcpServer?: { poolStart: string; poolEnd: string; leaseSeconds?: number }
}

export interface StaticRecord { id: string; hostname: string; address: string; comment?: string }

/// `GET /api/v1/dns`: the global servers, where lookups go, the static
/// records and the last resolution check.
export interface DnsDocument {
  global: { enabled: boolean; servers: string[] }
  mode: 'global' | 'interface'
  effective: { global?: string[]; links?: { name: string; servers: string[]; source: 'dhcp' | 'custom' | 'static' }[] }
  staticRecords: StaticRecord[]
  probe: DnsProbe | null
}

export interface DnsProbe { ok: boolean; name: string; latencyMs: number | null; checkedAt: string; detail?: string }

export interface DnsTestResult { server: string; reachable: boolean; latencyMs: number | null }

export interface WifiScan {
  available: boolean
  interface?: string
  /// Why no scan ran, when micad can say it in the operator's terms.
  reason?: 'access-point' | 'client-off'
  detail?: string
  networks?: { ssid: string; bssid: string; signalDbm?: number; frequencyMhz?: number; flags: string }[]
}

/// `GET /api/v1/containers`: the declared projects and the
/// engine's own reads, named apart because they answer different questions.
/// The engine half is mica-containerd's JSON verbatim, so only the fields this
/// console reads are named.
export interface ContainerOverview {
  enabled: boolean
  /// The engine the product carries: on docker nothing is declared, and the
  /// engine's view is every container dockerd holds.
  containerEngine?: 'podman' | 'docker'
  /// Each project's Compose text, by name.
  projects?: Record<string, string>
  /// Each project's directory, by name, for a project declared with one.
  projectDirs?: Record<string, string>
  /// mica-containerd's view.
  engine: {
    available: boolean
    detail?: string
    projects?: ContainerProjectObservation[]
  }
  images: { available: boolean; detail?: string; entries?: { Names?: string[] }[] }
}

/// One project as mica-containerd holds it, with a container per service.
export interface ContainerProjectObservation {
  name: string
  /// Where its relative host paths resolve.
  projectDir?: string
  services?: ContainerObservation[]
}

/// One container as mica-containerd observes it. Only the fields the console
/// reads; the daemon sends more.
export interface ContainerObservation {
  project?: string
  service?: string
  spec?: { name?: string; image?: string }
  desired?: string
  phase?: string
  health?: string
  restarts?: number
  exit_code?: number
  error?: string
}

/// `POST /api/v1/container-projects/{project}/check`.
export interface ComposeCheck {
  ok: boolean
  containers?: unknown[]
  detail?: string
  problems?: { service: string; field: string; detail: string }[]
}

/// `GET`/`PUT /api/v1/web`: who reaches the API, and where the console listens.
export interface WebConfiguration {
  mode: 'network' | 'local' | 'off'
  httpPort: number
  httpsEnabled: boolean
  httpsPort: number
}

/// One certificate as `/api/v1/web/certificate` describes it. Never the key.
export interface CertificateInfo {
  subject: string
  issuer: string
  selfSigned: boolean
  dnsNames: string[]
  ipAddresses: string[]
  notBefore: string
  notAfter: string
  sha256: string
}

/// `GET /api/v1/web/certificate`.
export interface CertificateStatus {
  serving: boolean
  certificate: CertificateInfo | null
}

export interface MqttConfiguration {
  enabled: boolean
  listen: { address: string; port: number }
  auth: { enabled: boolean }
}

/// `GET /api/v1/mqtt`: the declared subtree, and the `mqtt` reconciler's own
/// record of what it did with it. The observed half is micad's document
/// verbatim, so only the fields this console reads are named here.
export interface MqttOverview {
  configured: MqttConfiguration
  observed: {
    available: boolean
    state?: {
      configPath?: string
      listen?: { address?: string; port?: number }
      auth?: { enabled?: boolean }
      units?: { unit: string; activeState?: string; unitFileState?: string }[]
    }
    error?: string
  }
}

/// What a whole-map write would do, answered by `?dryRun=true`.
export interface NetworkDryRun {
  /// VLAN parents the write would declare, with the configuration they run.
  adopted: string[]
  warnings: string[]
}

export interface NetworkOverview {
  configured: Record<string, unknown>
  configuredCount: number
  /// Named as `baseVersion`, a write replaces only the map this read returned.
  version: string
  observed: {
    available: boolean
    interfaceCount: number
    interfaces: ObservedInterface[]
    error?: string
  }
}

export interface UiStatus {
  mode: 'builtIn' | 'custom'
  custom?: CustomUiDetails
  availableCustom?: CustomUiDetails & {
    usable: boolean
    unavailableReason?:
      | 'missingActivationRecord'
      | 'unsafeTree'
      | 'indexUnavailable'
      | 'manifestInvalid'
      | 'digestMismatch'
      | 'incompatible'
  }
}

export interface CustomUiDetails {
  generation: number
  indexReadable: boolean
  name?: string
  version?: string
  digestMatches?: boolean
  compatible?: boolean
}

export interface UiBundleDetails extends CustomUiDetails {
  digest?: string
  compressedBytes?: number
  expandedBytes?: number
  usable: boolean
  unavailableReason?: NonNullable<UiStatus['availableCustom']>['unavailableReason']
}

export interface UiBundleList {
  activeGeneration?: number
  bundles: UiBundleDetails[]
  retentionLimit: number
}

/// Every fact the platform surfaces carries its own availability: an absent
/// fact says why it is absent instead of reading as a healthy zero.
export interface AvailableFact {
  available: boolean
  detail?: string
}

/// `GET /api/v1/device/code`: the code printed on the label, or why there is none.
export type DeviceCode = { code: string, source: string } | { code: null, reason: string }

export interface TimeStatus {
  status: 'synchronized' | 'polling' | 'offline-degraded' | 'invalid-source' | 'unknown'
  synchronized?: boolean
  detail?: string
  server?: { name?: string | null; address?: string | null }
  sample?: {
    leap: number
    stratum: number
    spike: boolean
    offsetSeconds: number
    packetCount: number
    correction: 'step' | 'slew'
  }
}

export interface StorageSpace {
  totalBytes: number
  usedBytes: number
  freeBytes: number
  reservedBytes: number
  usedPercent: number
}

export interface StorageTier {
  name: string
  role: string
  partitionLabel: string
  expectedMount?: string
  present: boolean
  detail?: string
  device?: string
  partitionBytes?: number
  mounted?: boolean
  mount?: string
  filesystem?: string
  readOnly?: boolean
  space?: StorageSpace
  pressure?: 'normal' | 'warning' | 'critical'
  check: {
    recorded?: false
    unit?: string
    activeState?: string | null
    result?: string | null
    exitStatus?: number | null
  }
}

/// Absent wear is reported as unsupported with a reason, never omitted: a
/// medium nobody can read must not look healthy. The JEDEC estimate is a
/// bucket range and stays a range.
export interface StorageMedium {
  name: string
  kind: string
  sizeBytes?: number
  model?: string
  rotational?: boolean
  health:
    | {
        supported: true
        source: string
        raw: { lifeTime: string; preEolInfo?: string | null }
        lifetimeEstimates: { raw: string; usedPercentMin?: number; usedPercentMax?: number; detail?: string }[]
        preEol?: string | null
      }
    | { supported: false; reason: string }
}

/// A bind namespace of the DATA filesystem. It carries no capacity of its own
/// on purpose: /mica and /srv are two views of one pool, and a second capacity
/// here would invite a reader to add them together.
export interface StorageBind {
  name: string
  mount: string
  source: string
  owner: string
  readiness: 'ready' | 'degraded' | 'unavailable' | 'unknown'
  detail?: string
  mounted?: boolean
  device?: string
  readOnly?: boolean
  sourceOnData?: boolean
  sourceIsDirectory?: boolean
  probe?:
    | { attempted: true; passed: true }
    | { attempted: true; passed: false; error: string }
    | { attempted: false; reason: string }
}

export interface StorageStatus {
  tiers: StorageTier[]
  namespaces: {
    sharedCapacityTier: string
    detail: string
    binds: StorageBind[]
  }
  media: StorageMedium[]
  policy: {
    warningPercent: number
    warningClearPercent: number
    criticalPercent: number
    criticalClearPercent: number
    watchedTiers: string[]
  }
  lifecycle: Record<string, string>
}

export interface SystemInformation {
  machineId: AvailableFact & { id?: string }
  board: AvailableFact & { model?: string; source?: string }
  kernel: AvailableFact & { release?: string; version?: string }
  release: AvailableFact & {
    name?: string
    id?: string
    version?: string
    versionId?: string
    prettyName?: string
    buildId?: string
    imageId?: string
    imageVersion?: string
  }
  system: AvailableFact & {
    version?: string
    package?: string
    /** The pinned SOURCE_DATE_EPOCH every file in the image carries. */
    fileEpoch?: AvailableFact & { epoch?: number; date?: string }
  }
  daemon: AvailableFact & { name?: string; version?: string }
  packages: AvailableFact & {
    count?: number
    micaCount?: number
    malformedRows?: number
    truncated?: boolean
    entries?: { name: string; version: string; architecture: string; mica: boolean }[]
  }
  deployment: AvailableFact & {
    id?: string
    version?: string
    generation?: number
    kernelId?: string
    kernelRelease?: string
    rootfsId?: string
    confirmed?: boolean
    contentVerified?: boolean
    secureBoot?: boolean
    backend?: 'uefi' | 'uboot-fit'
    bootVerified?: boolean
  }
  uptime: AvailableFact & { seconds?: number }
}

export interface ThermalReading {
  sensor: string
  label?: string | null
  milliCelsius: number
}

export interface WatchdogDevice {
  device: string
  identity?: string
  state?: string
  timeoutSeconds?: number
  timeLeftSeconds?: number
  nowayout?: boolean
  bootstatus: AvailableFact & { raw?: number; flags?: string[] }
}

export interface SystemTelemetry {
  thermal: AvailableFact & { zones?: ThermalReading[]; hwmon?: ThermalReading[] }
  watchdog: AvailableFact & { devices?: WatchdogDevice[] }
  reset: AvailableFact & { reason: 'watchdog' | 'kernel-crash' | 'unknown' }
}

export interface ObservedAddress {
  family?: string
  address?: string
  prefixLength?: number
  scope?: string
  configSource?: string
}

export interface ObservedDhcp extends AvailableFact {
  state?: string
  inferred?: boolean
  lease?: {
    address?: string
    prefixLength?: number
    server?: string
    router?: string
    lifetimeSeconds?: number
  }
}

export interface ObservedNetworkInterface {
  name: string
  index?: number
  kind?: string
  type?: string
  driver?: string
  mtu?: number
  link: {
    administrativeState?: string
    operationalState?: string
    carrierState?: string
    carrier?: boolean
    onlineState?: string
    addressState?: string
  }
  hardwareAddress?: string
  addresses: ObservedAddress[]
  dhcp: ObservedDhcp
  dns: (string | { address?: string })[]
  /// False for a configured interface the device does not have.
  present?: boolean
  /// The DHCPv4 lease the interface holds.
  lease?: AvailableFact & { server?: string; router?: string; remainingSeconds?: number }
  linkDns?: { address: string; source?: string; provider?: string }[]
  routes?: { dest: string; via?: string; metric?: number; source?: string; table?: string | number }[]
  can?: AvailableFact & {
    busState?: string
    bitrate?: number
    samplePoint?: number
    txErrors?: number
    rxErrors?: number
    warningCount?: number
    errorPassiveCount?: number
    busOffCount?: number
    rx?: { packets?: number; dropped?: number; overruns?: number; errors?: number }
    tx?: { packets?: number; dropped?: number; errors?: number }
  }
  wireguard?: AvailableFact & {
    peers?: { publicKey: string; latestHandshake?: number | null; rxBytes?: number; txBytes?: number; persistentKeepalive?: number | null }[]
  }
  wifi?: AvailableFact & {
    state?: string
    associated?: boolean
    ssid?: string
    bssid?: string
    frequencyMhz?: number
    keyManagement?: string
    rssiDbm?: number
    linkSpeedMbps?: number
  }
}

export interface ObservedNetworkState {
  /// The interface this request arrived through, `null` when none holds its
  /// address, as behind a proxy.
  session?: { interface: string | null; localAddress?: string | null }
  interfaces: AvailableFact & { count?: number; entries?: ObservedNetworkInterface[] }
  defaultRoutes: AvailableFact & {
    count?: number
    entries?: {
      family?: string
      gateway?: string
      interface?: string
      interfaceIndex?: number
      metric?: number
      protocol?: string
      protocolId?: number
      table?: string
      tableId?: number
      configSource?: string
    }[]
  }
  dns: AvailableFact & {
    linkServers?: string[]
    resolverServers?: string[]
    probe?: { name: string; reachable: boolean; result: string; detail?: string }
  }
  wifi: AvailableFact & {
    associations?: {
      interface?: string
      state?: string
      associated?: boolean
      ssid?: string
      bssid?: string
      frequencyMhz?: number
      keyManagement?: string
      rssiDbm?: number
      linkSpeedMbps?: number
    }[]
    association?: AvailableFact & { interface?: string; ssid?: string; bssid?: string; signalDbm?: number; txBitrate?: number; security?: string }
  }
  /// What is associated with THIS device's access point, as hostapd reports
  /// it. `wifi.associations` is the other direction: what this device joined.
  accessPoint: AvailableFact & {
    running?: boolean
    /// Why the access point is not running: `disabled`, `client_mode`,
    /// `radio_blocked`, `country_unset`, `channel_invalid`, `driver_error`.
    /// While it runs: `acs_failed`, on the country's first channel.
    reason?: string | null
    reasonDetail?: string
    entries?: {
      interface: string
      stationCount: number
      stations: { mac: string; connectedSeconds?: number; signalDbm?: number; rxBytes?: number; txBytes?: number; address?: string; hostname?: string }[]
    }[]
  }
  capabilities: {
    wifi: {
      supported: boolean
      interfaces: string[]
      blocked?: boolean
      security?: string[]
      bands?: string[]
      acs?: boolean
      apClientConcurrent?: boolean
      channels?: number[]
    }
    bluetooth: { supported: boolean; adapters: string[] }
    cellular: { supported: boolean; interfaces: string[] }
  }
}

export interface SnapshotSummary {
  id: number
  bytes: number
  collectedAt?: string | null
  machineId?: string | null
  schemaVersion?: number | null
}

export interface SnapshotRetention {
  maxSnapshots: number
  maxTotalBytes: number
  maxSnapshotBytes: number
  schemaVersion: number
  redactionSchemaVersion: number
}

export interface SnapshotList {
  snapshots: SnapshotSummary[]
  retention: SnapshotRetention
}

export interface SnapshotCollected {
  snapshot: SnapshotSummary
  elapsedMillis: number
  sections: Record<string, string>
  droppedFields: number
  redactedFields: number
}

/// `GET /api/v1/units`: the user's own services, each one declared in the
/// settings or put there with `mica-unit`.
export interface UnitsOverview {
  init: 'systemd' | 'openrc'
  /// Whether this root has `mica-unit`; without it nothing can be declared.
  available: boolean
  units: UnitEntry[]
}

/// One service as micad reports it.
export interface UnitEntry {
  name: string
  declared: boolean
  /// micad applies it: declared, and for this device's init.
  managed: boolean
  installed: boolean
  /// Linked to start at boot.
  enabled: boolean
  /// The init's state, for an installed service.
  active?: string | null
  kind?: 'systemd' | 'openrc'
  /// Why a declared service is not applied.
  refused?: string
}

/// One declared service: what `services.units.<name>` holds.
export interface UserUnit {
  kind: 'systemd' | 'openrc'
  text: string
  conf?: string
  enabled: boolean
}

/// `GET /api/v1/units/{name}/logs`.
export interface UnitLog {
  source?: string
  available: boolean
  lines?: string[]
  truncated?: boolean
  detail?: string
}

/// Every app micad keeps.
export interface AppsOverview {
  /// Whether this daemon keeps apps at all.
  available: boolean
  arch: string
  apps: AppEntry[]
}

/// One installed app.
export interface AppEntry {
  name: string
  version: string
  description: string
  arch: string[]
  sha256: string
  /// `upload`, `cli`, `url` or `seed`.
  source: string
  installedAt: string
  /// The version a rollback returns to.
  previous: string | null
  units: { name: string; enabled: boolean; active: string | null }[]
  /// Root has the whole device; an app's own user only what it declares.
  privileges: { user: 'app' | 'root'; account: string | null; uid: number | null; capabilities: string[]; devices: string[] } | null
  last: { action: string; outcome: string; message?: string; at: string } | null
  update: { url: string; auto: boolean; available: string | null; checkedAt: string | null; error: string | null } | null
  seed: { version: string; policy: string | null } | null
}

/// An app's newest lines: each unit's log and the first-run output.
export interface AppLog {
  name: string
  units: (UnitLog & { unit: string })[]
  firstRun: string[] | null
}
