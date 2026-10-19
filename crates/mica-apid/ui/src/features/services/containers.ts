import type { ContainerObservation, ContainerOverview } from '@/lib/types'

/// What mica-containerd says about one container.
export interface Observed {
  /// mica-containerd's phase, absent when it did not answer or has not taken
  /// the declaration yet.
  state?: string
  /// The container's own health check, when it has one.
  health?: string
  /// How often the daemon restarted it.
  restarts?: number
  /// Its last exit code, meaningful once it has stopped on its own.
  exitCode?: number
  /// The image it runs.
  image?: string
  /// Why the daemon could not run it.
  error?: string
}

export interface ServiceRow extends Observed {
  service: string
}

export interface ProjectRow {
  name: string
  /// The declared Compose text, absent for a project mica-containerd still
  /// holds and the settings no longer declare: one it is removing.
  compose?: string
  /// Where its relative host paths resolve: the declared directory, else the
  /// one mica-containerd reports.
  projectDir?: string
  services: ServiceRow[]
}

function observed(entry: ContainerObservation): Observed {
  return {
    state: entry.phase,
    health: entry.health || undefined,
    restarts: entry.restarts,
    exitCode: entry.exit_code,
    image: entry.spec?.image,
    error: entry.error || undefined,
  }
}

/// Every project, declared or still held by mica-containerd, with a row per
/// service the daemon reports. A declared project the daemon has not taken
/// has no services yet.
export function projectRows(overview: ContainerOverview | undefined): ProjectRow[] {
  const rows = new Map<string, ProjectRow>()
  for (const [name, compose] of Object.entries(overview?.projects ?? {})) {
    rows.set(name, { name, compose, projectDir: overview?.projectDirs?.[name], services: [] })
  }
  for (const project of overview?.engine.projects ?? []) {
    const services = (project.services ?? [])
      .map((entry) => ({ service: entry.service || entry.spec?.name || '', ...observed(entry) }))
      .filter((row) => row.service)
      .sort((left, right) => left.service.localeCompare(right.service))
    const row: ProjectRow = rows.get(project.name) ?? { name: project.name, services: [] }
    rows.set(project.name, { ...row, projectDir: row.projectDir ?? project.projectDir, services })
  }
  return [...rows.values()].sort((left, right) => left.name.localeCompare(right.name))
}
