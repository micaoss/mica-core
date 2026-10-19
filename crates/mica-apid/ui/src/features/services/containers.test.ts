import { describe, expect, it } from 'vitest'
import { projectRows } from './containers'

describe('the project rows', () => {
  /// A declared project the daemon has not taken has no services yet, and one
  /// the daemon holds that the settings no longer declare is being removed.
  it('joins the declared projects and the daemon projects by name', () => {
    const rows = projectRows({
      enabled: true,
      projects: { app: 'services: {}\n', fresh: 'services: {}\n', site: 'services: {}\n' },
      projectDirs: { site: '/srv/site' },
      engine: {
        available: true,
        projects: [
          { name: 'site', projectDir: '/srv/site', services: [] },
          { name: 'app', projectDir: '/mica/volumes/app', services: [
            { project: 'app', service: 'web', spec: { name: 'app-web-1', image: 'docker.io/library/nginx:1' }, phase: 'running', restarts: 0 },
            { project: 'app', service: 'db', spec: { name: 'app-db-1' }, phase: 'fatal', exit_code: 1, error: 'image not found' },
          ] },
          { name: 'old', services: [{ project: 'old', service: 'job', phase: 'exited' }] },
        ],
      },
      images: { available: true, entries: [] },
    })

    expect(rows.map((row) => row.name)).toEqual(['app', 'fresh', 'old', 'site'])
    expect(rows[0].projectDir).toBe('/mica/volumes/app')
    expect(rows[3].projectDir).toBe('/srv/site')
    expect(rows[0].services.map((row) => row.service)).toEqual(['db', 'web'])
    expect(rows[0].services[0]).toMatchObject({ state: 'fatal', exitCode: 1, error: 'image not found' })
    expect(rows[0].services[1]).toMatchObject({ state: 'running', image: 'docker.io/library/nginx:1' })
    expect(rows[1]).toEqual({ name: 'fresh', compose: 'services: {}\n', services: [] })
    expect(rows[2].compose).toBeUndefined()
  })

  it('has nothing to show before the device answers', () => {
    expect(projectRows(undefined)).toEqual([])
  })
})
