import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api, type SessionStatus } from '@/shared/lib/http'
import type { NetworkOverview, ObservedNetworkState, WifiAccessPoint } from '@/lib/types'
import { sessionKey } from '@/features/auth/auth'
import { useFeatures, useOpenrc } from '@/shared/lib/features'
import { useChangeSet } from './change-set'
import { pendingChanges, type ChangeSet, type NetworkMap } from './model'
import { buildView, sessionInterface, type WifiView } from './view'

export interface WifiClientRole { enabled: boolean; interface: string }

/// Everything the network pages read, and the change set staged on top of it.
export function useNetwork() {
  const serves = useFeatures()
  const openrc = useOpenrc()
  const network = useQuery({ queryKey: ['network'], queryFn: () => api<NetworkOverview>('/api/v1/network'), refetchInterval: 10_000 })
  const status = useQuery({ queryKey: ['observed-network'], queryFn: () => api<ObservedNetworkState>('/api/v1/network/status'), refetchInterval: 15_000, retry: false })
  const hostname = useQuery({ queryKey: ['settings', 'hostname'], queryFn: () => api<string>('/api/v1/settings/hostname') })
  const session = useQuery<SessionStatus>({ queryKey: sessionKey, enabled: false })
  const client = useQuery({ queryKey: ['wifi-client'], queryFn: () => api<WifiClientRole>('/api/v1/wifi/client'), enabled: serves('wifi'), retry: false })
  const ap = useQuery({ queryKey: ['wifi-ap'], queryFn: () => api<WifiAccessPoint>('/api/v1/wifi/ap'), enabled: serves('wifi'), retry: false })

  const configured = network.data?.configured as NetworkMap | undefined
  const changes = useChangeSet(hostname.data, configured)
  const set: ChangeSet | undefined = changes.set

  const wifi = useMemo<WifiView>(() => wifiView(client.data, ap.data, status.data), [client.data, ap.data, status.data])
  const applied = useMemo(() => buildView(set?.base ?? configured ?? {}, status.data, wifi), [set?.base, configured, status.data, wifi])
  const staged = useMemo(() => buildView(set?.staged ?? configured ?? {}, status.data, wifi), [set?.staged, configured, status.data, wifi])
  const kindOf = (name: string) => staged.ifaces[name]?.kind ?? applied.ifaces[name]?.kind ?? 'physical'
  const pending = set ? pendingChanges(set, kindOf, staged.order) : []

  return {
    network,
    status,
    hostname: hostname.data,
    openrc,
    serves,
    client,
    ap,
    wifi,
    set,
    conflicts: changes.conflicts,
    applying: changes.applying,
    applied,
    staged,
    pending,
    session: sessionInterface(status.data, session.data?.localAddress),
    localAddress: session.data?.localAddress,
  }
}

export type NetworkData = ReturnType<typeof useNetwork>

/// The radio's role, from the two roles that can hold it.
export function wifiView(client: WifiClientRole | undefined, ap: WifiAccessPoint | undefined, status: ObservedNetworkState | undefined): WifiView {
  const apOn = ap !== undefined && ap.mode !== 'off'
  const radio = apOn ? ap.interface : client?.interface ?? ap?.interface ?? status?.capabilities.wifi.interfaces[0]
  const point = (status?.accessPoint?.entries ?? []).find((entry) => entry.interface === radio)
  const association = (status?.wifi.associations ?? []).find((entry) => entry.interface === radio && entry.associated)
  const running = status?.accessPoint?.running
  return {
    radio,
    mode: apOn ? 'ap' : client?.enabled ? 'client' : 'off',
    apRunning: running ?? point !== undefined,
    apReason: running === false ? status?.accessPoint.reason ?? undefined : undefined,
    apSsid: ap?.ssid,
    stations: point?.stationCount ?? 0,
    association: association ? { ssid: association.ssid, bssid: association.bssid, rssiDbm: association.rssiDbm, linkSpeedMbps: association.linkSpeedMbps } : undefined,
  }
}
