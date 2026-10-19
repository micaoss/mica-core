import { describe, expect, it } from 'vitest'
import { band, groupScan, type ScannedBss } from './wifi-scan'

const bss = (ssid: string, bssid: string, signalDbm?: number, frequencyMhz?: number): ScannedBss =>
  ({ ssid, bssid, signalDbm, frequencyMhz, flags: '[WPA2-PSK-CCMP][ESS]' })

describe('the scan, grouped by network', () => {
  it('makes one network of every access point with its SSID, the strongest first', () => {
    const groups = groupScan([
      bss('mesh', 'aa:00:00:00:00:01', -71, 2437),
      bss('cafe', 'bb:00:00:00:00:01', -50, 2412),
      bss('mesh', 'aa:00:00:00:00:02', -44, 5180),
      bss('mesh', 'aa:00:00:00:00:03', undefined, 5955),
    ])

    expect(groups.map((group) => [group.ssid, group.accessPoints.length])).toEqual([['mesh', 3], ['cafe', 1]])
    expect(groups[0].best.bssid).toBe('aa:00:00:00:00:02')
    expect(groups[0].accessPoints.map((ap) => ap.bssid)).toEqual(['aa:00:00:00:00:02', 'aa:00:00:00:00:01', 'aa:00:00:00:00:03'])
  })

  it('keeps each hidden access point apart', () => {
    const groups = groupScan([bss('', 'cc:00:00:00:00:01', -60), bss('', 'cc:00:00:00:00:02', -55)])

    expect(groups.map((group) => group.accessPoints.length)).toEqual([1, 1])
    expect(groups[0].best.bssid).toBe('cc:00:00:00:00:02')
  })

  it('names the band of a frequency', () => {
    expect([2412, 2484, 5180, 5825, 5955, 7115, 60480, undefined].map(band)).toEqual(['2.4', '2.4', '5', '5', '6', '6', undefined, undefined])
  })
})
