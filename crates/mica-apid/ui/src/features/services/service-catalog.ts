import { Boxes, RadioTower } from 'lucide-react'

export interface ServiceDefinition {
  id: 'containers' | 'mqtt'
  icon: typeof Boxes
  settingsPath: string
  statePath: string
}

export const serviceCatalog: readonly ServiceDefinition[] = [
  { id: 'containers', icon: Boxes, settingsPath: 'container.enabled', statePath: 'container' },
  { id: 'mqtt', icon: RadioTower, settingsPath: 'mqtt.enabled', statePath: 'mqtt' },
]

export function findService(id: string) {
  return serviceCatalog.find((service) => service.id === id)
}

/// What a service's reconciler published, read by the keys that reconciler
/// writes: `serviceState` for the container engine's unit, and for MQTT the
/// broker unit's `activeState` and the listener it was bound to. Nothing is
/// assumed: a key the document does not carry yields nothing, and the
/// listener is only an endpoint while the broker is active.
export function serviceObserved(id: ServiceDefinition['id'], state: Record<string, unknown> | undefined): { state?: string; endpoint?: string } {
  if (!state) return {}
  if (id === 'containers') {
    return { state: text(state.serviceState) }
  }
  const units = Array.isArray(state.units) ? state.units as Record<string, unknown>[] : []
  const broker = text(units.find((unit) => unit.unit === 'mica-mqtt-broker.service')?.activeState)
  const listen = state.listen as { address?: unknown; port?: unknown } | undefined
  const address = text(listen?.address)
  const endpoint = broker === 'active' && address && typeof listen?.port === 'number' ? `${address}:${listen.port}` : undefined
  return { state: broker, endpoint }
}

function text(value: unknown) {
  return typeof value === 'string' && value.length > 0 ? value : undefined
}
