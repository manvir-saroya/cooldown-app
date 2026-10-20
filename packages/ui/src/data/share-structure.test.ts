import { describe, expect, it } from 'vitest'
import { shareStructure } from './share-structure'

describe('shareStructure', () => {
  it('returns the previous value when nothing changed', () => {
    const prev = [{ id: 'a', nested: { n: 1, list: [1, 2] } }, { id: 'b', nested: { n: 2, list: [] } }]
    const next = structuredClone(prev)
    expect(shareStructure(prev, next)).toBe(prev)
  })

  it('reuses unchanged elements and fields of a changed value', () => {
    const prev = [{ id: 'a', stats: { n: 1 } }, { id: 'b', stats: { n: 2 } }]
    const next = [{ id: 'a', stats: { n: 1 } }, { id: 'b', stats: { n: 3 } }]
    const shared = shareStructure(prev, next)
    expect(shared).not.toBe(prev)
    expect(shared[0]).toBe(prev[0])
    expect(shared[1]).not.toBe(prev[1])
    expect(shared).toEqual(next)
  })

  it('treats a changed length, a missing key and an added key as changes', () => {
    expect(shareStructure([1, 2], [1, 2, 3])).toEqual([1, 2, 3])
    expect(shareStructure([1, 2, 3], [1, 2])).toEqual([1, 2])
    const prev = { a: 1, b: undefined }
    expect(shareStructure(prev, { a: 1 })).not.toBe(prev)
    expect(shareStructure({ a: 1 }, { a: 1, b: 2 })).toEqual({ a: 1, b: 2 })
  })

  it('takes the new value when the shapes differ or there is nothing before', () => {
    expect(shareStructure(undefined, [1])).toEqual([1])
    expect(shareStructure({ a: 1 }, [1])).toEqual([1])
    expect(shareStructure([1], 'text')).toBe('text')
    expect(shareStructure(null, null)).toBeNull()
  })
})
