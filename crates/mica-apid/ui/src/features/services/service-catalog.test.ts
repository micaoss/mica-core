import { describe, expect, it } from 'vitest'
import { findService, serviceCatalog, serviceObserved } from './service-catalog'

describe('the service catalog', () => {
  it('lists the services in its order', () => {
    expect(serviceCatalog.map((service) => service.id)).toEqual(['containers', 'mqtt'])
  })

  /// Every service in the catalogue is one the device can be asked about:
  /// a card with no settings path could only ever report an invented state.
  it('gives every service a device setting and a state path', () => {
    expect(findService('containers')?.settingsPath).toBe('container.enabled')
    expect(serviceCatalog.every((service) => service.settingsPath && service.statePath)).toBe(true)
    expect(findService('nothing')).toBeUndefined()
  })
})

describe('what a service reports', () => {
  const mqtt = (activeState: string) => ({
    enabled: true,
    listen: { address: '0.0.0.0', port: 1883 },
    units: [
      { unit: 'mica-mqtt-broker.service', activeState, unitFileState: 'enabled' },
      { unit: 'mica-mqttd.service', activeState: 'inactive', unitFileState: 'enabled' },
    ],
  })

  it('reads the keys each reconciler publishes', () => {
    expect(serviceObserved('containers', { enabled: true, service: 'docker.service', serviceState: 'active' })).toEqual({ state: 'active' })
    expect(serviceObserved('mqtt', mqtt('active'))).toEqual({ state: 'active', endpoint: '0.0.0.0:1883' })
  })

  /// A listener from the last published state describes a broker that has
  /// stopped, so it is not shown as the endpoint.
  it('reports no endpoint unless the broker is active', () => {
    expect(serviceObserved('mqtt', mqtt('failed'))).toEqual({ state: 'failed', endpoint: undefined })
  })

  it('reports nothing the device did not publish', () => {
    expect(serviceObserved('containers', undefined)).toEqual({})
    expect(serviceObserved('containers', { state: 'running' })).toEqual({ state: undefined })
    expect(serviceObserved('mqtt', { enabled: false })).toEqual({ state: undefined, endpoint: undefined })
  })
})
