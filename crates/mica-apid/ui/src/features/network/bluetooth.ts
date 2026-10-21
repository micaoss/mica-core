import type { BluetoothOverview } from '@/lib/types'

/// The phone app's code a device offers until it is claimed.
export const DEFAULT_PIN = '000000'

export interface BluetoothRow {
  address: string
  name: string
  /// The adapter currently sees it. `false` for a paired device out of range.
  seen: boolean
  connected: boolean
  trusted: boolean
  blocked: boolean
}

/// The paired devices: the trust list, joined by address with what the
/// adapter reports of each. A device in range that nobody paired is not one,
/// and is not shown.
export function bluetoothRows(overview: BluetoothOverview | undefined): BluetoothRow[] {
  const seen = new Map((overview?.devices.entries ?? []).map((device) => [device.address, device]))
  return Object.entries(overview?.declared ?? {})
    .map(([address, declared]) => {
      const device = seen.get(address)
      return {
        address,
        // The adapter's name wins: it is what the device calls itself now,
        // while the declaration records what it called itself when it paired.
        name: device?.name || declared.name,
        seen: device !== undefined,
        connected: device?.connected ?? false,
        trusted: declared.trusted,
        blocked: declared.blocked,
      }
    })
    .sort((left, right) => (left.name || left.address).localeCompare(right.name || right.address))
}

/// `m:ss` of the seconds left until `expiresAt`, never below zero.
export function remaining(expiresAt: number, now: number): string {
  const seconds = Math.max(0, Math.round(expiresAt - now / 1000))
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
}
