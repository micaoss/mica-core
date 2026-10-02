import { describe, expect, it } from 'vitest'
import { bluetoothRows } from './bluetooth'

const overview = {
  enabled: true,
  discoverable: false,
  pin: '4211',
  declared: {
    'AA:BB:CC:DD:EE:01': { name: 'phone', trusted: true, blocked: false },
    'AA:BB:CC:DD:EE:02': { name: 'headset', trusted: true, blocked: false },
  },
  adapter: { available: true },
  devices: {
    available: true,
    entries: [
      { address: 'AA:BB:CC:DD:EE:01', name: 'phone (new name)', paired: true, trusted: true, blocked: false, connected: true, rssi: -55 },
      { address: 'AA:BB:CC:DD:EE:03', name: 'speaker', paired: false, trusted: false, blocked: false, connected: false, rssi: -70 },
    ],
  },
  pending: null,
}

describe('the bluetooth rows', () => {
  /// Both directions: a paired device out of range is declared and not seen,
  /// and a device in range that nobody paired is seen and not declared.
  it('joins the trust list and what the adapter sees', () => {
    const rows = bluetoothRows(overview)

    const byAddress = Object.fromEntries(rows.map((row) => [row.address, row]))
    expect(byAddress['AA:BB:CC:DD:EE:01']).toMatchObject({ declared: true, seen: true, connected: true, rssi: -55 })
    expect(byAddress['AA:BB:CC:DD:EE:02']).toMatchObject({ declared: true, seen: false })
    expect(byAddress['AA:BB:CC:DD:EE:03']).toMatchObject({ declared: false, seen: true })
  })

  /// The adapter's name wins: it is what the device calls itself now, while
  /// the declaration records what it called itself when it paired.
  it('prefers the name the adapter reports over the recorded one', () => {
    const rows = bluetoothRows(overview)

    expect(rows.find((row) => row.address === 'AA:BB:CC:DD:EE:01')?.name).toBe('phone (new name)')
  })

  it('puts the trusted devices first', () => {
    const rows = bluetoothRows(overview)

    expect(rows.map((row) => row.declared)).toEqual([true, true, false])
  })

  it('has nothing to show before the device answers', () => {
    expect(bluetoothRows(undefined)).toEqual([])
  })
})
