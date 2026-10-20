import type { WifiScan } from '@/lib/types'

export type ScannedBss = NonNullable<WifiScan['networks']>[number]

/// One network on the air: every access point that answers to its SSID, the
/// strongest first. A mesh or a building's access points are one network, as
/// other systems show them.
export interface ScannedNetwork {
  ssid: string
  /// The strongest access point: its signal and security are the network's.
  best: ScannedBss
  accessPoints: ScannedBss[]
}

/// The band a frequency is in, or undefined when the radio did not say.
export function band(frequencyMhz?: number): '2.4' | '5' | '6' | undefined {
  if (frequencyMhz === undefined) return undefined
  if (frequencyMhz >= 2400 && frequencyMhz < 2500) return '2.4'
  if (frequencyMhz >= 4900 && frequencyMhz < 5925) return '5'
  if (frequencyMhz >= 5925 && frequencyMhz <= 7125) return '6'
  return undefined
}

const strength = (bss: ScannedBss) => bss.signalDbm ?? Number.NEGATIVE_INFINITY

/// The scan's access points grouped by SSID, each group's strongest first and
/// the groups by their strongest. A hidden network (no SSID) stays one row per
/// access point: an empty name does not make two networks one.
export function groupScan(networks: ScannedBss[] = []): ScannedNetwork[] {
  const groups = new Map<string, ScannedBss[]>()
  for (const bss of networks) {
    const key = bss.ssid ? `ssid:${bss.ssid}` : `bssid:${bss.bssid}`
    groups.set(key, [...(groups.get(key) ?? []), bss])
  }
  return [...groups.values()]
    .map((accessPoints) => {
      const sorted = [...accessPoints].sort((left, right) => strength(right) - strength(left) || left.bssid.localeCompare(right.bssid))
      return { ssid: sorted[0].ssid, best: sorted[0], accessPoints: sorted }
    })
    .sort((left, right) => strength(right.best) - strength(left.best) || left.ssid.localeCompare(right.ssid))
}

/// What joining a network takes, read from the supplicant's scan flags. The
/// client joins WPA2 and WPA3 networks with a passphrase (`key_mgmt=WPA-PSK
/// SAE`) and open ones; an 802.1X (EAP) or WEP network it cannot join at all.
export type ScanSecurity = 'open' | 'psk' | 'sae' | 'eap' | 'wep'

export function scanSecurity(flags: string): ScanSecurity {
  if (flags.includes('PSK')) return 'psk'
  if (flags.includes('SAE')) return 'sae'
  if (flags.includes('EAP')) return 'eap'
  if (flags.includes('WEP')) return 'wep'
  return 'open'
}
