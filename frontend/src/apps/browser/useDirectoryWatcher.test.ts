import { effectScope, nextTick, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useDirectoryWatcher } from './useDirectoryWatcher'

class FakeEventSource {
  static instances: FakeEventSource[] = []
  static readonly CLOSED = 2
  readyState = 0
  closed = false
  private readonly listeners = new Map<string, Array<() => void>>()

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this)
  }

  addEventListener(type: string, listener: () => void): void {
    const list = this.listeners.get(type) ?? []
    list.push(listener)
    this.listeners.set(type, list)
  }

  close(): void {
    this.closed = true
    this.readyState = FakeEventSource.CLOSED
  }

  emit(type: string): void {
    for (const listener of this.listeners.get(type) ?? []) listener()
  }

  open(): void {
    this.readyState = 1
    this.emit('open')
  }

  failClosed(): void {
    this.readyState = FakeEventSource.CLOSED
    this.emit('error')
  }
}

let visibility: DocumentVisibilityState = 'visible'

function setVisibility(next: DocumentVisibilityState): void {
  visibility = next
  document.dispatchEvent(new Event('visibilitychange'))
}

function mountWatcher(storageId = 'primary', path = '/') {
  const storage = ref(storageId)
  const currentPath = ref(path)
  const onChange = vi.fn()
  const scope = effectScope()
  scope.run(() => useDirectoryWatcher(storage, currentPath, onChange))
  return { storage, path: currentPath, onChange, scope }
}

beforeEach(() => {
  FakeEventSource.instances = []
  visibility = 'visible'
  Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => visibility })
  vi.stubGlobal('EventSource', FakeEventSource)
  vi.useFakeTimers()
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('directory watcher', () => {
  it('subscribes exactly once after the storage id resolves', async () => {
    const watcher = mountWatcher('')
    try {
      expect(FakeEventSource.instances).toHaveLength(0)
      watcher.storage.value = 'primary'
      await nextTick()
      expect(FakeEventSource.instances).toHaveLength(1)
    } finally { watcher.scope.stop() }
  })

  it('closes the old stream when the directory changes', async () => {
    const watcher = mountWatcher()
    try {
      await nextTick()
      const first = FakeEventSource.instances[0]!
      watcher.path.value = '/images'
      await nextTick()
      expect(first.closed).toBe(true)
      expect(FakeEventSource.instances).toHaveLength(2)
    } finally { watcher.scope.stop() }
  })

  it('closes the stream when the scope goes away', async () => {
    const watcher = mountWatcher()
    await nextTick()
    watcher.scope.stop()
    expect(FakeEventSource.instances[0]!.closed).toBe(true)
  })

  it('resumes after the tab returns even when a reconnect timer was pending', async () => {
    const watcher = mountWatcher()
    try {
      await nextTick()
      FakeEventSource.instances[0]!.failClosed()
      setVisibility('hidden')
      await vi.advanceTimersByTimeAsync(60_000)
      expect(FakeEventSource.instances).toHaveLength(1)
      setVisibility('visible')
      await nextTick()
      expect(FakeEventSource.instances).toHaveLength(2)
    } finally { watcher.scope.stop() }
  })

  it('stops reconnecting after three closed attempts', async () => {
    const watcher = mountWatcher()
    try {
      await nextTick()
      for (let attempt = 0; attempt < 6; attempt++) {
        FakeEventSource.instances.at(-1)!.failClosed()
        await vi.advanceTimersByTimeAsync(60_000)
      }
      expect(FakeEventSource.instances).toHaveLength(4)
    } finally { watcher.scope.stop() }
  })

  it('gives a new directory a fresh reconnect budget', async () => {
    const watcher = mountWatcher()
    try {
      await nextTick()
      for (let attempt = 0; attempt < 4; attempt++) {
        FakeEventSource.instances.at(-1)!.failClosed()
        await vi.advanceTimersByTimeAsync(60_000)
      }
      expect(FakeEventSource.instances).toHaveLength(4)
      watcher.path.value = '/other'
      await nextTick()
      expect(FakeEventSource.instances).toHaveLength(5)
      FakeEventSource.instances[4]!.failClosed()
      await vi.advanceTimersByTimeAsync(60_000)
      expect(FakeEventSource.instances).toHaveLength(6)
    } finally { watcher.scope.stop() }
  })

  it('coalesces a burst of change signals into one refresh', async () => {
    const watcher = mountWatcher()
    try {
      await nextTick()
      const source = FakeEventSource.instances[0]!
      source.emit('change')
      source.emit('change')
      await vi.advanceTimersByTimeAsync(300)
      expect(watcher.onChange).toHaveBeenCalledTimes(1)
    } finally { watcher.scope.stop() }
  })
})
