import { describe, expect, it } from 'vitest'
import { candidatesFromDrop, candidatesFromFiles, joinUploadPath } from './uploadQueue'

describe('uploadQueue', () => {
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
})
