export interface UploadCandidate {
  file: File
  relativePath: string
}

// Shared with retained task admission; directories also consume selection budget.
export const MAX_UPLOAD_QUEUE_TASKS = 20_000

export class UploadSelectionLimitError extends Error {
  constructor() { super('Too many selected upload entries') }
}

function cleanRelativePath(path: string): string {
  return path.replaceAll('\\', '/').split('/').filter(part => part && part !== '.').join('/')
}

export function joinUploadPath(directory: string, relativePath: string): string {
  return [cleanRelativePath(directory), cleanRelativePath(relativePath)].filter(Boolean).join('/')
}

export function candidatesFromFiles(files: Iterable<File>, limit = MAX_UPLOAD_QUEUE_TASKS): UploadCandidate[] {
  const candidates: UploadCandidate[] = []
  let count = 0
  for (const file of files) {
    if (++count > limit) throw new UploadSelectionLimitError()
    const relativePath = cleanRelativePath(file.webkitRelativePath || file.name)
    if (relativePath) candidates.push({ file, relativePath })
  }
  return candidates
}

// Native callbacks cannot be cancelled, but waiting can stop immediately.
function readEntry<T>(request: (success: (value: T) => void, failure: (error: DOMException) => void) => void, signal?: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    function finish(error: unknown, value?: T): void {
      signal?.removeEventListener('abort', abort)
      if (error !== undefined) reject(error)
      else resolve(value as T)
    }
    function abort(): void { finish(signal?.reason ?? new DOMException('Aborted', 'AbortError')) }
    if (signal?.aborted) { abort(); return }
    signal?.addEventListener('abort', abort, { once: true })
    try { request(value => finish(undefined, value), error => finish(error)) }
    catch (error) { finish(error) }
  })
}

export async function candidatesFromDrop(
  transfer: DataTransfer,
  options: { signal?: AbortSignal; limit?: number } = {},
): Promise<UploadCandidate[]> {
  const { signal, limit = MAX_UPLOAD_QUEUE_TASKS } = options
  signal?.throwIfAborted()
  const roots: FileSystemEntry[] = []
  for (const item of transfer.items) {
    if (item.kind !== 'file') continue
    const entry = item.webkitGetAsEntry?.()
    if (!entry) continue
    if (roots.length >= limit) throw new UploadSelectionLimitError()
    roots.push(entry)
  }
  // Read DataTransfer synchronously, while the browser's drop data is available.
  if (!roots.length) return candidatesFromFiles(transfer.files, limit)
  let count = roots.length
  const pending: Array<FileSystemEntry | FileSystemDirectoryReader> = roots.reverse()
  const candidates: UploadCandidate[] = []
  while (pending.length) {
    signal?.throwIfAborted()
    const next = pending.pop()!
    if ('readEntries' in next) {
      const chunk = await readEntry<FileSystemEntry[]>((success, failure) => next.readEntries(success, failure), signal)
      count += chunk.length
      if (count > limit) throw new UploadSelectionLimitError()
      if (chunk.length) {
        pending.push(next)
        for (let index = chunk.length - 1; index >= 0; index--) pending.push(chunk[index]!)
      }
    } else if (next.isDirectory) {
      pending.push((next as FileSystemDirectoryEntry).createReader())
    } else if (next.isFile) {
      const file = await readEntry<File>((success, failure) => (next as FileSystemFileEntry).file(success, failure), signal)
      signal?.throwIfAborted()
      const relativePath = cleanRelativePath(next.fullPath || file.name)
      if (relativePath) candidates.push({ file, relativePath })
    }
  }
  return candidates
}
