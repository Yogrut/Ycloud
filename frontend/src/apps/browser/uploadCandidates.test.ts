import { describe, expect, it, vi } from 'vitest'
import { candidatesFromDrop, candidatesFromFiles, joinUploadPath, UploadSelectionLimitError } from './uploadCandidates'

function fileEntry(path: string): FileSystemFileEntry {
  return {
    isFile: true, isDirectory: false, fullPath: path, name: path.split('/').pop()!,
    file: vi.fn((success: (file: File) => void) => success(new File(['data'], path.split('/').pop()!))),
  } as unknown as FileSystemFileEntry
}

function directory(chunks: FileSystemEntry[][]): FileSystemDirectoryEntry {
  return {
    isFile: false, isDirectory: true,
    createReader: () => ({ readEntries: vi.fn((success: (entries: FileSystemEntry[]) => void) => success(chunks.shift() ?? [])) }),
  } as unknown as FileSystemDirectoryEntry
}

function transfer(entries: FileSystemEntry[]): DataTransfer {
  return { items: entries.map(entry => ({ kind: 'file', webkitGetAsEntry: () => entry })), files: [] } as unknown as DataTransfer
}

describe('uploadCandidates', () => {
  it('keeps selected folder paths while normalizing separators', () => {
    const file = new File(['hello'], 'hello.txt')
    Object.defineProperty(file, 'webkitRelativePath', { value: 'docs\\notes/hello.txt' })

    expect(candidatesFromFiles([file])).toEqual([{ file, relativePath: 'docs/notes/hello.txt' }])
    expect(joinUploadPath('/current/', '/docs/notes/hello.txt')).toBe('current/docs/notes/hello.txt')
  })

  it('reads every directory chunk from a dropped folder', async () => {
    const first = new File(['a'], 'a.txt')
    const second = new File(['b'], 'b.txt')
    const fileEntry = (file: File, fullPath: string): FileSystemFileEntry => ({
      isFile: true,
      isDirectory: false,
      name: file.name,
      fullPath,
      filesystem: {} as FileSystem,
      file: success => success(file),
      getParent: () => undefined,
    })
    const chunks = [[fileEntry(first, '/folder/a.txt')], [fileEntry(second, '/folder/b.txt')], []]
    const directory = {
      isFile: false,
      isDirectory: true,
      name: 'folder',
      fullPath: '/folder',
      filesystem: {} as FileSystem,
      createReader: () => ({ readEntries: success => success(chunks.shift() ?? []) }),
      getParent: () => undefined,
    } as FileSystemDirectoryEntry
    const transfer = {
      items: [{ kind: 'file', webkitGetAsEntry: () => directory }],
      files: [],
    } as unknown as DataTransfer

    const result = await candidatesFromDrop(transfer)
    expect(result.map(item => item.relativePath)).toEqual(['folder/a.txt', 'folder/b.txt'])
  })

  it('preserves depth-first order across roots, folders and chunks', async () => {
    const nested = directory([[fileEntry('/root/nested/first.txt')], []])
    const root = directory([[nested, fileEntry('/root/second.txt')], [fileEntry('/root/third.txt')], []])
    const candidates = await candidatesFromDrop(transfer([root, fileEntry('/last.txt')]))
    expect(candidates.map(candidate => candidate.relativePath)).toEqual([
      'root/nested/first.txt', 'root/second.txt', 'root/third.txt', 'last.txt',
    ])
  })

  it('starts only one native file read at a time', async () => {
    const first = fileEntry('/first.txt')
    const second = fileEntry('/second.txt')
    let complete!: (file: File) => void
    first.file = vi.fn(success => { complete = success })
    const pending = candidatesFromDrop(transfer([first, second]))
    expect(first.file).toHaveBeenCalledTimes(1)
    expect(second.file).not.toHaveBeenCalled()
    complete(new File(['first'], 'first.txt'))
    expect((await pending).map(candidate => candidate.relativePath)).toEqual(['first.txt', 'second.txt'])
    expect(second.file).toHaveBeenCalledTimes(1)
  })

  it('rejects oversized roots before reading any file', async () => {
    const first = fileEntry('/first.txt')
    await expect(candidatesFromDrop(transfer([first, fileEntry('/second.txt')]), { limit: 1 }))
      .rejects.toBeInstanceOf(UploadSelectionLimitError)
    expect(first.file).not.toHaveBeenCalled()
  })

  it('counts directories and rejects an oversized chunk before opening its files', async () => {
    const first = fileEntry('/first.txt')
    const root = directory([[first, fileEntry('/second.txt')], []])
    await expect(candidatesFromDrop(transfer([root]), { limit: 2 })).rejects.toBeInstanceOf(UploadSelectionLimitError)
    expect(first.file).not.toHaveBeenCalled()
  })

  it('rejects an oversized later chunk without returning partial candidates', async () => {
    const first = fileEntry('/first.txt')
    const second = fileEntry('/second.txt')
    const root = directory([[first], [second], []])
    await expect(candidatesFromDrop(transfer([root]), { limit: 2 })).rejects.toBeInstanceOf(UploadSelectionLimitError)
    expect(first.file).toHaveBeenCalledTimes(1)
    expect(second.file).not.toHaveBeenCalled()
  })

  it('stops a selected file iterator as soon as its limit is exceeded', () => {
    let visited = 0
    function* files() {
      for (let index = 0; index < 10; index++) {
        visited++
        yield new File([], `${index}.txt`)
      }
    }
    expect(() => candidatesFromFiles(files(), 2)).toThrow(UploadSelectionLimitError)
    expect(visited).toBe(3)
    expect(candidatesFromFiles([new File([], 'one.txt'), new File([], 'two.txt')], 2)).toHaveLength(2)
  })

  it('falls back to selected files when entry access is unavailable', async () => {
    const file = new File([], 'one.txt')
    for (const items of [[], [{ kind: 'string' }], [{ kind: 'file', webkitGetAsEntry: () => null }], [{ kind: 'file' }]]) {
      const drop = { items, files: [file] } as unknown as DataTransfer
      expect(await candidatesFromDrop(drop)).toEqual([{ file, relativePath: 'one.txt' }])
    }
  })

  it('rejects an already-aborted drop before using DataTransfer', async () => {
    const controller = new AbortController()
    controller.abort()
    const entry = fileEntry('/first.txt')
    await expect(candidatesFromDrop(transfer([entry]), { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' })
    expect(entry.file).not.toHaveBeenCalled()
  })

  it('cancels a pending directory read and ignores its late chunk', async () => {
    const controller = new AbortController()
    const child = fileEntry('/late.txt')
    let complete!: (entries: FileSystemEntry[]) => void
    const root = {
      isFile: false, isDirectory: true,
      createReader: () => ({ readEntries: (success: (entries: FileSystemEntry[]) => void) => { complete = success } }),
    } as unknown as FileSystemDirectoryEntry
    const pending = candidatesFromDrop(transfer([root]), { signal: controller.signal })
    controller.abort()
    await expect(pending).rejects.toMatchObject({ name: 'AbortError' })
    complete([child])
    await Promise.resolve()
    expect(child.file).not.toHaveBeenCalled()
  })

  it('propagates a native read failure without opening the next file', async () => {
    const first = fileEntry('/first.txt')
    const second = fileEntry('/second.txt')
    first.file = vi.fn((_success, failure) => failure!(new DOMException('read failed')))
    await expect(candidatesFromDrop(transfer([first, second]))).rejects.toThrow('read failed')
    expect(second.file).not.toHaveBeenCalled()
  })

  it('propagates a synchronous reader failure', async () => {
    const root = {
      isFile: false, isDirectory: true,
      createReader: () => ({ readEntries: () => { throw new Error('reader failed') } }),
    } as unknown as FileSystemDirectoryEntry
    await expect(candidatesFromDrop(transfer([root]))).rejects.toThrow('reader failed')
  })

  it('removes cancellation listeners when reading finishes', async () => {
    const controller = new AbortController()
    const remove = vi.spyOn(controller.signal, 'removeEventListener')
    await candidatesFromDrop(transfer([fileEntry('/first.txt')]), { signal: controller.signal })
    expect(remove).toHaveBeenCalledWith('abort', expect.any(Function))
    controller.abort()
  })

  it('keeps budgets independent across drop collections', async () => {
    const first = await candidatesFromDrop(transfer([fileEntry('/one.txt')]), { limit: 1 })
    const second = await candidatesFromDrop(transfer([fileEntry('/two.txt')]), { limit: 1 })
    expect(first[0]!.relativePath).toBe('one.txt')
    expect(second[0]!.relativePath).toBe('two.txt')
  })
})
