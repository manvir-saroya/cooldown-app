/**
 * Structural sharing for refetched data.
 *
 * Every poll the source hands back freshly parsed copies of the same readings. Returning `prev` where
 * `next` is equal (and reusing any equal element or field inside a changed value) lets React skip the
 * render, and lets memoised components skip the cards that did not change.
 *
 * Only plain JSON-like data is walked (objects, arrays, primitives), which is all a source returns.
 */
export function shareStructure<T>(prev: unknown, next: T): T {
  if (Object.is(prev, next)) return next
  if (Array.isArray(next)) {
    if (!Array.isArray(prev)) return next
    const merged = next.map((item, i) => shareStructure(prev[i], item))
    return (merged.length === prev.length && merged.every((item, i) => item === prev[i]) ? prev : merged) as T
  }
  if (isPlainObject(next)) {
    if (!isPlainObject(prev)) return next
    const keys = Object.keys(next)
    const merged: Record<string, unknown> = {}
    for (const key of keys) merged[key] = shareStructure(prev[key], next[key])
    const same = keys.length === Object.keys(prev).length && keys.every((key) => key in prev && merged[key] === prev[key])
    return (same ? prev : merged) as T
  }
  return next
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  if (typeof value !== 'object' || value === null) return false
  const proto: unknown = Object.getPrototypeOf(value)
  return proto === Object.prototype || proto === null
}
