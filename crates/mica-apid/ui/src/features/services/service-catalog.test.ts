import { describe, expect, it } from 'vitest'
import { findService, serviceCatalog, serviceEndpoint } from './service-catalog'

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

describe('the endpoint a service reports', () => {
  it('reads the first endpoint-shaped key the state document carries', () => {
    expect(serviceEndpoint({ endpoint: 'unix:///run/containerd.sock' })).toBe('unix:///run/containerd.sock')
    expect(serviceEndpoint({ state: 'running', listen: '0.0.0.0:1883' })).toBe('0.0.0.0:1883')
  })

  /// The prototype prints a socket path for every service. Printing one the
  /// device never reported would be inventing a device fact.
  it('reports nothing when the device named no endpoint', () => {
    expect(serviceEndpoint({ state: 'running' })).toBeUndefined()
    expect(serviceEndpoint(undefined)).toBeUndefined()
    expect(serviceEndpoint({ endpoint: '' })).toBeUndefined()
  })
})
