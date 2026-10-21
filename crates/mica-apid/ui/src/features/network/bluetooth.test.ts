import { describe, expect, it } from 'vitest'
import { bluetoothRows, remaining } from './bluetooth'

const overview = {
  enabled: true,
  declared: {
    'AA:BB:CC:DD:EE:01': { name: 'phone', trusted: true, blocked: false },
    'AA:BB:CC:DD:EE:02': { name: 'headset', trusted: true, blocked: true },
  },
  adapter: { available: true },
  devices: {
    available: true,
    entries: [
      { address: 'AA:BB:CC:DD:EE:01', name: 'phone (new name)', paired: true, trusted: true, blocked: false, connected: true, rssi: -55 },
      { address: 'AA:BB:CC:DD:EE:03', name: 'speaker', paired: false, trusted: false, blocked: false, connected: false, rssi: -70 },
    ],
  },
  pending: [],
  pairWindow: null,
}

describe('the bluetooth rows', () => {
  /// A paired device out of range is still paired; a device in range that
  /// nobody paired is not shown.
  it('lists the paired devices with what the adapter sees of them', () => {
    const rows = bluetoothRows(overview)

    expect(rows.map((row) => row.address)).toEqual(['AA:BB:CC:DD:EE:02', 'AA:BB:CC:DD:EE:01'])
    expect(rows[1]).toMatchObject({ seen: true, connected: true, name: 'phone (new name)' })
    expect(rows[0]).toMatchObject({ seen: false, blocked: true, name: 'headset' })
  })

  it('has nothing to show before the device answers', () => {
    expect(bluetoothRows(undefined)).toEqual([])
  })

  it('counts the window down in minutes and seconds', () => {
    expect(remaining(1_000_180, 1_000_000_000)).toBe('3:00')
    expect(remaining(1_000_065, 1_000_000_000)).toBe('1:05')
    expect(remaining(1_000_000, 1_000_005_000)).toBe('0:00')
  })
})
