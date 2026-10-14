import type { BluetoothOverview } from '@/lib/types'

export interface BluetoothRow {
  address: string
  name: string
  /// The device is in the declared trust list.
  declared: boolean
  /// The adapter currently sees it. `false` for a declared device that is out
  /// of range, which is a different thing from one that was never paired.
  seen: boolean
  connected: boolean
  rssi?: number
}

/// Every device this page has anything to say about: the trust list and what
/// the adapter sees, joined by address.
///
/// Both directions matter. A paired device out of range is declared and not
/// seen; a device in range that nobody paired is seen and not declared, and
/// pairing with it is the one action that changes that.
export function bluetoothRows(overview: BluetoothOverview | undefined): BluetoothRow[] {
  const rows = new Map<string, BluetoothRow>()
  for (const [address, device] of Object.entries(overview?.declared ?? {})) {
    rows.set(address, { address, name: device.name, declared: true, seen: false, connected: false })
  }
  for (const device of overview?.devices.entries ?? []) {
    const existing = rows.get(device.address)
    rows.set(device.address, {
      address: device.address,
      // The adapter's name wins: it is what the device calls itself now,
      // while the declaration records what it called itself when it paired.
      name: device.name || existing?.name || '',
      declared: existing?.declared ?? false,
      seen: true,
      connected: device.connected,
      rssi: device.rssi ?? undefined,
    })
  }
  return [...rows.values()].sort((left, right) => {
    // Declared first, then by name, then by address: the devices an operator
    // already trusts are the ones they came to the page for.
    if (left.declared !== right.declared) return left.declared ? -1 : 1
    return (left.name || left.address).localeCompare(right.name || right.address)
  })
}
