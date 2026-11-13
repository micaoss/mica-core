import { cleanup, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { ExternalVolume, StorageStatus } from '@/lib/types'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { StoragePanel } from './storage-panel'

function status(overrides: Partial<StorageStatus> = {}): StorageStatus {
  return {
    tiers: [
      {
        name: 'data', role: 'data', partitionLabel: 'DATA', present: true, device: '/dev/mmcblk0p8',
        mounted: true, mount: '/mnt/data', filesystem: 'ext4', readOnly: false,
        space: { totalBytes: 8_589_934_592, usedBytes: 4_294_967_296, freeBytes: 4_294_967_296, reservedBytes: 429_496_729, usedPercent: 50 },
        pressure: 'normal',
        check: { unit: 'systemd-fsck@dev-mmcblk0p8.service', result: 'success', exitStatus: 0 },
      },
      {
        name: 'boot', role: 'boot', partitionLabel: 'BOOT', present: false,
        detail: 'this board boots from SPI', check: { recorded: false },
      },
    ],
    namespaces: {
      sharedCapacityTier: 'data',
      detail: '/mica and /srv are binds of the DATA filesystem',
      binds: [
        { name: 'mica', mount: '/mica', source: '/mnt/data/mica', owner: 'system', readiness: 'ready', mounted: true, sourceOnData: true, sourceIsDirectory: true, probe: { attempted: true, passed: true } },
        { name: 'srv', mount: '/srv', source: '/mnt/data/srv', owner: 'user', readiness: 'ready', mounted: true, sourceOnData: true, sourceIsDirectory: true, probe: { attempted: true, passed: true } },
      ],
    },
    media: [
      {
        name: 'mmcblk0', kind: 'mmc', sizeBytes: 31_268_536_320, model: 'SDINBDA4',
        health: {
          supported: true, source: '/sys/class/mmc_host',
          raw: { lifeTime: '0x02 0x01', preEolInfo: '0x01' },
          lifetimeEstimates: [{ raw: '0x02', usedPercentMin: 10, usedPercentMax: 20 }],
          preEol: 'normal',
        },
      },
    ],
    policy: {
      warningPercent: 85, warningClearPercent: 80, criticalPercent: 95, criticalClearPercent: 90,
      watchedTiers: ['data'],
    },
    lifecycle: { backupRestore: 'unsupported', secureErase: 'unsupported' },
    ...overrides,
  }
}

const NO_MEDIA = { generation: '0', items: [] }

function volume(overrides: Partial<ExternalVolume> = {}): ExternalVolume {
  return {
    id: 'v1', deviceId: 'd1', label: 'UPDATE', transport: 'usb', filesystem: 'vfat',
    sizeBytes: 16_106_127_360, availableBytes: 8_053_063_680, state: 'mounted_readonly', busy: false, reason: null,
    ...overrides,
  }
}

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('storage status', () => {
  it('states the pool once, on the tier that owns it, and never per bind', async () => {
    stubFetch({ '/api/v1/storage/media': NO_MEDIA, '/api/v1/storage/status': status() })
    renderPanel(<StoragePanel />)

    expect(await screen.findByText(/4.0 GiB of 8.0 GiB used/)).toBeTruthy()
    expect(screen.getByText(/Both are views of one filesystem and share its capacity, reported once on the data tier/)).toBeTruthy()
    // A per-bind size bar would be a lie: /mica and /srv are two views of one
    // pool, and a second number here invites a reader to add them together.
    for (const mount of ['/mica', '/srv']) {
      const row = screen.getByText(mount).closest('div')!
      expect(row.textContent).not.toMatch(/GiB|MiB|%/)
    }
    expect(screen.getAllByText(/of 8.0 GiB used/)).toHaveLength(1)
  })

  it('reports an absent tier, its reserved pool and check evidence', async () => {
    stubFetch({ '/api/v1/storage/media': NO_MEDIA, '/api/v1/storage/status': status() })
    renderPanel(<StoragePanel />)

    expect(await screen.findByText('not present on this board')).toBeTruthy()
    expect(screen.getByText(/409.6 MiB reserved for root/)).toBeTruthy()
    expect(screen.getByText(/last check: success \(exit 0\)/)).toBeTruthy()
  })

  it('uses the exact readiness words for a degraded and an unavailable bind', async () => {
    stubFetch({
      '/api/v1/storage/media': NO_MEDIA,
      '/api/v1/storage/status': status({
        namespaces: {
          sharedCapacityTier: 'data',
          detail: '/mica and /srv are binds of the DATA filesystem',
          binds: [
            { name: 'mica', mount: '/mica', source: '/mnt/data/mica', owner: 'system', readiness: 'degraded', mounted: true, readOnly: true, sourceOnData: true, probe: { attempted: false, reason: 'the mount is read-only' } },
            { name: 'srv', mount: '/srv', source: '/srv', owner: 'user', readiness: 'unavailable', mounted: true, sourceOnData: false, probe: { attempted: true, passed: false, error: 'EROFS' } },
          ],
        },
      }),
    })
    renderPanel(<StoragePanel />)

    expect(await screen.findByText(/degraded — mounted on DATA but not fully writable/)).toBeTruthy()
    expect(screen.getByText(/no write probe: the mount is read-only/)).toBeTruthy()
    expect(screen.getByText(/unavailable — nothing may be written here/)).toBeTruthy()
    expect(screen.getByText(/its mount is not backed by DATA \(\/srv\)/)).toBeTruthy()
    expect(screen.getByText(/write probe failed: EROFS/)).toBeTruthy()
  })

  it('shows eMMC wear as the JEDEC bucket range, not one averaged percentage', async () => {
    stubFetch({ '/api/v1/storage/media': NO_MEDIA, '/api/v1/storage/status': status() })
    renderPanel(<StoragePanel />)

    expect(await screen.findByText(/lifetime used: 10–20% \(10% resolution\)/)).toBeTruthy()
    expect(screen.getByText(/end-of-life indicator: normal/)).toBeTruthy()
    expect(screen.queryByText(/lifetime used: 15%/)).toBeNull()
  })

  it('states unsupported wear with its reason and an empty media list as empty', async () => {
    stubFetch({
      '/api/v1/storage/media': NO_MEDIA,
      '/api/v1/storage/status': status({
        media: [{ name: 'sda', kind: 'scsi', health: { supported: false, reason: 'this medium exports no wear counters' } }],
      }),
    })
    const first = renderPanel(<StoragePanel />)
    expect(await screen.findByText(/wear reporting unsupported: this medium exports no wear counters/)).toBeTruthy()
    first.unmount()

    stubFetch({ '/api/v1/storage/media': NO_MEDIA, '/api/v1/storage/status': status({ media: [] }) })
    renderPanel(<StoragePanel />)
    expect(await screen.findByText('no media reported')).toBeTruthy()
  })

  it('names each lifecycle decision explicitly', async () => {
    stubFetch({ '/api/v1/storage/media': NO_MEDIA, '/api/v1/storage/status': status() })
    renderPanel(<StoragePanel />)

    expect(await screen.findByText('Backup and restore')).toBeTruthy()
    expect(screen.getAllByText('not supported')).toHaveLength(2)
  })

  it('surfaces a read failure as an error', async () => {
    stubFetch({ '/api/v1/storage/media': NO_MEDIA, '/api/v1/storage/status': () => jsonResponse({ error: { message: 'micad is not reachable' } }, 503) })
    renderPanel(<StoragePanel />)

    expect(await screen.findByText('micad is not reachable')).toBeTruthy()
  })
})

describe('user mounts', () => {
  it('lists the mounts in user places and the swaps, marking what the user declared', async () => {
    stubFetch({
      '/api/v1/storage/media': NO_MEDIA,
      '/api/v1/storage/status': status({
        userMounts: [
          { target: '/srv/backup', source: '/dev/sdb1', filesystem: 'ext4', options: 'rw,nosuid,nodev,noatime', user: true, totalBytes: 2_147_483_648, usedBytes: 1_073_741_824 },
          { target: '/var/mnt/media', source: 'systemd-1', filesystem: 'autofs', options: 'rw,relatime', user: false },
        ],
        swaps: [{ source: '/dev/sdb2', kind: 'partition', sizeBytes: 1_073_741_824, usedBytes: 0, user: true }],
      }),
    })
    renderPanel(<StoragePanel />)

    const backup = (await screen.findByText('/srv/backup')).closest('li, div')!
    expect(within(backup as HTMLElement).getByText('yours')).toBeTruthy()
    expect(screen.getByText(/\/dev\/sdb1 · ext4 · rw,nosuid,nodev,noatime · 1.0 GiB used of 2.0 GiB/)).toBeTruthy()
    // No space for an automount: reading it would mount it.
    expect(screen.getByText('systemd-1 · autofs · rw,relatime')).toBeTruthy()
    expect(screen.getByText('not declared')).toBeTruthy()
    expect(screen.getByText('/dev/sdb2')).toBeTruthy()
    expect(screen.getByText(/partition · 0 B used of 1.0 GiB/)).toBeTruthy()
  })

  it('says when nothing is mounted in user places', async () => {
    stubFetch({ '/api/v1/storage/media': NO_MEDIA, '/api/v1/storage/status': status({ userMounts: [], swaps: [] }) })
    renderPanel(<StoragePanel />)

    expect(await screen.findByText('Nothing is mounted in the places you may mount, and no swap is active.')).toBeTruthy()
  })
})

describe('external media', () => {
  const media = (items: ExternalVolume[]) => ({ '/api/v1/storage/status': status(), '/api/v1/storage/media': { generation: '1', items } })

  it('groups volumes by device with their state, reason and free space', async () => {
    stubFetch(media([
      volume(),
      volume({ id: 'v2', label: null, filesystem: 'ext4', availableBytes: null, state: 'failed', reason: 'needs_repair' }),
      volume({ id: 'v3', deviceId: 'd2', transport: 'sd', label: 'CAMERA', filesystem: 'exfat', availableBytes: null, state: 'unsupported', reason: 'filesystem_not_in_kernel' }),
    ]))
    renderPanel(<StoragePanel />)

    const usb = await screen.findByRole('region', { name: 'USB storage' })
    expect(within(usb).getByText(/vfat · 15.0 GiB · 7.5 GiB free/)).toBeTruthy()
    expect(within(usb).getByText('mounted read-only')).toBeTruthy()
    expect(within(usb).getByText('No label')).toBeTruthy()
    expect(within(usb).getByText(/the filesystem needs repair on another computer/)).toBeTruthy()
    // Only the mounted volume has something to unmount.
    expect(within(usb).getAllByRole('button', { name: 'Unmount' })).toHaveLength(1)
    const sd = screen.getByRole('region', { name: 'SD card' })
    expect(within(sd).getByText(/this device has no driver for the filesystem/)).toBeTruthy()
    expect(within(sd).queryByRole('button', { name: 'Unmount' })).toBeNull()
    expect(within(sd).getByRole('button', { name: 'Eject' })).toBeTruthy()
  })

  it('unmounts one volume after a confirmation', async () => {
    const fetch = stubFetch({ ...media([volume()]), 'POST /api/v1/storage/media/v1/unmount': { item: volume({ state: 'unmounted', availableBytes: null }) } })
    renderPanel(<StoragePanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Unmount' }))
    const dialog = await screen.findByRole('alertdialog')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Unmount' }))

    expect(await screen.findByText('Volume unmounted')).toBeTruthy()
    expect(fetch).toHaveBeenCalledWith('/api/v1/storage/media/v1/unmount', expect.objectContaining({ method: 'POST' }))
  })

  it('reports a refused eject in the device\'s words, and an ejected device as safe to unplug', async () => {
    stubFetch({
      ...media([volume()]),
      'POST /api/v1/storage/media/devices/d1/eject': () => jsonResponse({ error: { code: 'media_device_busy', message: 'something uses this media device' } }, 409),
    })
    const first = renderPanel(<StoragePanel />)
    await userEvent.click(await screen.findByRole('button', { name: 'Eject' }))
    const dialog = await screen.findByRole('alertdialog')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Eject' }))
    expect(await screen.findByText('something uses this media device')).toBeTruthy()
    first.unmount()

    stubFetch(media([volume({ state: 'ejected', availableBytes: null })]))
    renderPanel(<StoragePanel />)
    expect(await screen.findByText('safe to unplug')).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Eject' })).toBeNull()
  })

  it('says when nothing is plugged in', async () => {
    stubFetch(media([]))
    renderPanel(<StoragePanel />)

    expect(await screen.findByText('No USB stick or SD card is plugged in.')).toBeTruthy()
  })
})
