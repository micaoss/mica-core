import { describe, expect, it } from 'vitest'
import {
  containerRows,
  formatEnvironment,
  formatPorts,
  formatVolumes,
  parseEnvironment,
  parseList,
  parsePorts,
  parseVolumes,
} from './containers'

describe('the container rows', () => {
  /// Both directions: a declared container the daemon has not taken has not
  /// started, and one it still holds that nobody declares is something an
  /// operator needs to see rather than something the console omits.
  it('joins the declared map and the daemon list by name, keeping both sides', () => {
    const rows = containerRows({
      enabled: true,
      declared: {
        'node-red': { image: 'docker.io/nodered/node-red:4.0.9' },
        metrics: { image: 'docker.io/library/busybox:1' },
      },
      engine: {
        available: true,
        entries: [
          { spec: { name: 'node-red' }, phase: 'running', health: 'healthy', restarts: 2, exit_code: 0 },
          { spec: { name: 'stray' }, phase: 'exited', health: '', restarts: 0, exit_code: 137 },
        ],
      },
      images: { available: true, entries: [] },
    })

    expect(rows.map((row) => row.name)).toEqual(['metrics', 'node-red', 'stray'])
    expect(rows[0].state).toBeUndefined()
    expect(rows[1]).toMatchObject({ state: 'running', health: 'healthy', restarts: 2 })
    expect(rows[2].declared).toBeUndefined()
    expect(rows[2]).toMatchObject({ state: 'exited', exitCode: 137 })
    expect(rows[2].health).toBeUndefined()
  })

  it('has nothing to show before the device answers', () => {
    expect(containerRows(undefined)).toEqual([])
  })
})

describe('the container form fields', () => {
  it('round-trips ports, volumes and the environment', () => {
    const ports = parsePorts('8080:80, 5353:53/udp')
    expect(ports).toEqual([
      { host: 8080, container: 80, protocol: 'tcp' },
      { host: 5353, container: 53, protocol: 'udp' },
    ])
    expect(formatPorts(ports)).toBe('8080:80, 5353:53/udp')

    const volumes = parseVolumes('/mica/apps/app:/data, /mica/certs:/certs:ro')
    expect(volumes).toEqual([
      { host: '/mica/apps/app', container: '/data', readOnly: false },
      { host: '/mica/certs', container: '/certs', readOnly: true },
    ])
    expect(formatVolumes(volumes)).toBe('/mica/apps/app:/data, /mica/certs:/certs:ro')

    const environment = parseEnvironment('TZ=UTC, LOG=debug')
    expect(environment).toEqual({ TZ: 'UTC', LOG: 'debug' })
    expect(formatEnvironment(environment)).toBe('TZ=UTC, LOG=debug')
  })

  /// Half-typed input is dropped rather than sent: the device would accept an
  /// empty-valued variable or a port with no number and the operator meant
  /// neither.
  it('drops what is not a complete entry', () => {
    expect(parsePorts('nonsense, 8080')).toEqual([{ host: 8080, container: 8080, protocol: 'tcp' }])
    expect(parseVolumes('/mica/only-a-host-path')).toEqual([])
    expect(parseEnvironment('TZ, =empty')).toEqual({})
    expect(parseList(' sleep  infinity ')).toEqual(['sleep', 'infinity'])
    expect(parseList('')).toEqual([])
  })
})
