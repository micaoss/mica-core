import { useEffect, useState } from 'react'
import type { BleProvisioning, BluetoothOverview } from '@/lib/types'
import { api } from '@/shared/lib/http'

/// Where a phone provisions the device from.
export const PROVISIONING_PAGE = 'setup-test.micaos.dev'

/// ONE query for BLE provisioning, shared by the Bluetooth tab and the Wi-Fi
/// tab's lock banner.
export const provisioningQuery = {
  queryKey: ['ble-provisioning'] as const,
  queryFn: () => api<BleProvisioning>('/api/v1/bluetooth/provisioning'),
}

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

/// Eight digits as two groups of four.
export function groupCode(code: string): string {
  return `${code.slice(0, 4)} ${code.slice(4)}`
}

/// The time now, ticking every second while `running`.
export function useClock(running: boolean) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!running) return
    setNow(Date.now())
    const timer = globalThis.setInterval(() => setNow(Date.now()), 1_000)
    return () => globalThis.clearInterval(timer)
  }, [running])
  return now
}
