import { describe, expect, it } from 'vitest'
import { updateView } from './update-view'

const running = 'a'.repeat(64)
const offered = { deploymentId: 'b'.repeat(64), version: '0.0.7' }

describe('the update view model', () => {
  it('keeps whether an update is on offer apart from what the device is doing', () => {
    expect(updateView({ lifecycle: { state: 'idle' } })).toEqual({ availability: 'not_checked', operation: 'idle' })
    expect(updateView({ lifecycle: { state: 'idle', last_check: '2026-11-11T06:00:00Z' } })).toEqual({ availability: 'up_to_date', operation: 'idle' })
    expect(updateView({ lifecycle: { state: 'checking' } })).toEqual({ availability: 'checking', operation: 'idle' })
    expect(updateView({ lifecycle: { state: 'idle', last_check: 'x', available: offered } })).toEqual({ availability: 'available', operation: 'idle' })
    expect(updateView({ lifecycle: { state: 'downloading', available: offered } })).toEqual({ availability: 'available', operation: 'downloading' })
    expect(updateView({ lifecycle: { state: 'installing', available: offered } })).toEqual({ availability: 'available', operation: 'installing' })
  })

  it('says a failed check is a failed check and not a failed install', () => {
    expect(updateView({ lifecycle: { state: 'failed', failedDuring: 'check' } })).toEqual({ availability: 'check_failed', operation: 'idle' })
    expect(updateView({ lifecycle: { state: 'failed', failedDuring: 'fetch', last_check: 'x', available: offered } })).toEqual({ availability: 'available', operation: 'failed' })
    expect(updateView({ lifecycle: { state: 'idle', last_check: 'x' }, install: { status: 'failed' } }).operation).toBe('failed')
  })

  it('follows an installed update through its reboot and its trial', () => {
    const state = { current: running, candidate: 'c'.repeat(64), fallback: null, failed: [] }
    expect(updateView({ lifecycle: { state: 'reboot-required' }, boot: { deploymentId: running }, state }).operation).toBe('restart_required')
    const trial = { ...state, current: 'd'.repeat(64), candidate: running }
    expect(updateView({ lifecycle: { state: 'ready' }, boot: { deploymentId: running }, state: trial }).operation).toBe('trial_boot')
    const core = { pending: { id: 'e'.repeat(64) }, boot: { coreSetId: 'e'.repeat(64) } }
    expect(updateView({ lifecycle: { state: 'idle' }, boot: { deploymentId: running }, state: { ...state, candidate: null }, sets: { core } }).operation).toBe('trial_boot')
    expect(updateView({ lifecycle: { state: 'idle' }, boot: { deploymentId: running }, state: { ...state, candidate: null } }).operation).toBe('idle')
  })
})
