export interface TaskAccepted {
  taskId: string
  task?: { id: string; status: string }
}

export interface TaskRecord {
  id: string
  operation: string
  dotPath: string
  source: string
  status: string
  enqueuedAt: string
  startedAt?: string
  finishedAt?: string
  outcome?: string
  message?: string
  foldedCount: number
}

export interface Meta {
  api: string
  settingsSchemaVersion: number
  daemon: string
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
/// BlueZ holds, the pairing code and whatever is waiting to be confirmed.
/// Declared and observed are named apart because they answer different
/// questions.
export interface BluetoothOverview {
  enabled: boolean
  discoverable: boolean
  /// The code a legacy peer is answered with. Displayed by design: somebody
  /// has to type it on the other device.
  pin: string
  declared: Record<string, { name: string; trusted: boolean; blocked: boolean }>
  adapter: AvailableFact & {
    address?: string
    alias?: string
    powered?: boolean
    discoverable?: boolean
    discovering?: boolean
  }
  devices: AvailableFact & { entries?: BluetoothDevice[] }
  /// The passkey waiting for a decision, or null.
  pending: { address: string; passkey: number; since: number } | null
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
}

export interface WifiScan {
  available: boolean
  interface?: string
  detail?: string
  networks?: { ssid: string; bssid: string; signalDbm?: number; frequencyMhz?: number; flags: string }[]
}

export interface ContainerPort { host: number; container: number; protocol: 'tcp' | 'udp' }
export interface ContainerVolume { host: string; container: string; readOnly: boolean }

export interface ContainerUnit {
  image: string
  command?: string[]
  environment?: Record<string, string>
  publish?: ContainerPort[]
  volumes?: ContainerVolume[]
  restart?: 'no' | 'on-failure' | 'always'
  autoStart?: boolean
}

/// `GET /api/v1/containers`: the declared map and the engine's own reads,
/// named apart because they answer different questions. The engine half is
/// podman's JSON verbatim, so only the fields this console reads are named.
export interface ContainerOverview {
  enabled: boolean
  declared: Record<string, ContainerUnit>
  engine: { available: boolean; detail?: string; entries?: { Names?: string[]; State?: string; Image?: string }[] }
  images: { available: boolean; detail?: string; entries?: { Names?: string[] }[] }
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

export interface NetworkOverview {
  configured: Record<string, unknown>
  configuredCount: number
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
  }
  /// What is associated with THIS device's access point, as hostapd reports
  /// it. `wifi.associations` is the other direction: what this device joined.
  accessPoint: AvailableFact & {
    entries?: {
      interface: string
      stationCount: number
      stations: { mac: string; connectedSeconds?: number; signalDbm?: number; rxBytes?: number; txBytes?: number }[]
    }[]
  }
  capabilities: {
    wifi: { supported: boolean; interfaces: string[] }
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
