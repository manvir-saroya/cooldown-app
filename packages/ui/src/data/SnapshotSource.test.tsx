import { act, render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import type { Snapshot } from '@cooldown/snapshot-schema'
import { SampleSnapshotSource } from './SampleSnapshotSource'
import { SnapshotSourceProvider, useSnapshots, type Query } from './SnapshotSource'

const NOW = Date.parse('2026-10-10T12:00:00Z')

/** A source whose snapshots can be swapped, like a poll that brings new readings. */
class SwapSource extends SampleSnapshotSource {
  calls = 0
  failNext = false
  current: Snapshot[] | undefined
  constructor() {
    super({ now: () => NOW })
  }
  override async getSnapshots(): Promise<Snapshot[]> {
    this.calls++
    if (this.failNext) {
      this.failNext = false
      throw new Error('adapter down')
    }
    // A fresh copy every time, as an IPC call returns: equal content, new objects.
    return structuredClone(this.current ?? (await super.getSnapshots()))
  }
  override subscribe(listener: () => void) {
    this.watchers.add(listener)
    return () => void this.watchers.delete(listener)
  }
  readonly watchers = new Set<() => void>()
  change() {
    for (const l of [...this.watchers]) l()
  }
}

function setup() {
  const source = new SwapSource()
  const seen: Query<Snapshot[]>[] = []
  function Consumer() {
    const query = useSnapshots()
    seen.push(query)
    return <p>{query.data ? `${query.data.length} readings` : 'loading'}</p>
  }
  render(
    <SnapshotSourceProvider source={source}>
      <Consumer />
    </SnapshotSourceProvider>,
  )
  return { source, seen }
}

describe('useSnapshots on repeated change events', () => {
  it('keeps the same data and does not render again when a refetch brings nothing new', async () => {
    const { source, seen } = setup()
    expect(await screen.findByText('7 readings')).toBeInTheDocument()
    const first = seen[seen.length - 1]!.data
    const renders = seen.length

    for (let i = 0; i < 5; i++) {
      await act(async () => {
        source.change()
      })
    }

    expect(source.calls).toBe(6)
    expect(seen.length).toBe(renders)
    expect(seen[seen.length - 1]!.data).toBe(first)
  })

  it('reuses the unchanged readings when only one changes, so memoised cards skip rendering', async () => {
    const { source, seen } = setup()
    await screen.findByText('7 readings')
    const before = seen[seen.length - 1]!.data!
    source.current = structuredClone(before)
    source.current[2] = { ...source.current[2]!, observedAt: new Date(NOW + 1000).toISOString() }

    await act(async () => {
      source.change()
    })

    const after = seen[seen.length - 1]!.data!
    expect(after).not.toBe(before)
    expect(after[2]).not.toBe(before[2])
    for (const i of [0, 1, 3, 4, 5, 6]) expect(after[i]).toBe(before[i])
  })

  it('keeps the last good data when a refetch fails, and clears the error when the next one works', async () => {
    const { source, seen } = setup()
    await screen.findByText('7 readings')
    const good = seen[seen.length - 1]!.data

    source.failNext = true
    await act(async () => {
      source.change()
    })
    const failed = seen[seen.length - 1]!
    expect(failed.error?.message).toBe('adapter down')
    expect(failed.data).toBe(good)

    await act(async () => {
      source.change()
    })
    const recovered = seen[seen.length - 1]!
    expect(recovered.error).toBeUndefined()
    expect(recovered.data).toBe(good)
  })
})
