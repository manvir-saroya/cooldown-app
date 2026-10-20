/**
 * What the two sources that talk to a Rust backend share: the desktop window's `TauriSnapshotSource`
 * and the Android app's `PhoneSnapshotSource`. Running a command, turning its plain-string rejection
 * into an Error, and one `cooldown://changed` listener that serves every subscriber.
 *
 * The Tauri API itself is passed in by each app (this package does not depend on it), which is also
 * how tests replace it.
 */

/** Emitted by the Rust side whenever anything the UI shows may have changed. It means "refetch". */
export const CHANGED_EVENT = 'cooldown://changed'

export type InvokeFn = <T>(command: string, args?: Record<string, unknown>) => Promise<T>
/** Resolves with the function that stops listening. */
export type ListenFn = (event: string, handler: () => void) => Promise<() => void>

/** Commands reject with the backend's sentence as a plain string; the UI wants an Error. */
function asError(err: unknown): Error {
  if (err instanceof Error) return err
  return new Error(typeof err === 'string' ? err : 'Something went wrong in the app.')
}

export abstract class TauriBackedSource {
  private readonly listeners = new Set<() => void>()
  private unlisten: Promise<() => void> | undefined

  protected constructor(
    private readonly invokeFn: InvokeFn,
    protected readonly listenFn: ListenFn,
  ) {}

  protected async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    try {
      return await this.invokeFn<T>(command, args)
    } catch (err) {
      throw asError(err)
    }
  }

  /** A call that changes data the backend does not always announce itself (hook, devices, pairing). */
  protected async mutate(command: string, args?: Record<string, unknown>): Promise<void> {
    await this.call<void>(command, args)
    this.emit()
  }

  protected emit(): void {
    for (const listener of [...this.listeners]) listener()
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener)
    this.unlisten ??= this.listenFn(CHANGED_EVENT, () => this.emit())
    return () => {
      this.listeners.delete(listener)
      if (this.listeners.size === 0 && this.unlisten) {
        const pending = this.unlisten
        this.unlisten = undefined
        void pending.then((stop) => stop(), () => {})
      }
    }
  }
}
