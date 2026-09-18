export type UploadTaskStatus = 'preparing' | 'queued' | 'uploading' | 'paused' | 'verifying' | 'succeeded' | 'failed' | 'cancelled'

export interface UploadCandidate {
  file: File
  relativePath: string
}

export interface UploadTask extends UploadCandidate {
  id: number
  storageId: string
  basePath: string
  targetPath: string
  ticket?: string
  status: UploadTaskStatus
  loaded: number
  error: string
  retryBlocked?: boolean
  cancelRequested?: boolean
  pauseRequested?: boolean
}

function cleanRelativePath(path: string): string {
  return path.replaceAll('\\', '/').split('/').filter(part => part && part !== '.').join('/')
}

export function candidatesFromFiles(files: Iterable<File>): UploadCandidate[] {
  return Array.from(files, file => ({
    file,
    relativePath: cleanRelativePath(file.webkitRelativePath || file.name),
  })).filter(candidate => candidate.relativePath.length > 0)
}

function readFileEntry(entry: FileSystemFileEntry): Promise<File> {
  return new Promise((resolve, reject) => entry.file(resolve, reject))
}

function readDirectoryChunk(reader: FileSystemDirectoryReader): Promise<FileSystemEntry[]> {
  return new Promise((resolve, reject) => reader.readEntries(resolve, reject))
}

async function readDirectoryEntries(entry: FileSystemDirectoryEntry): Promise<FileSystemEntry[]> {
  const reader = entry.createReader()
  const entries: FileSystemEntry[] = []
  while (true) {
    const chunk = await readDirectoryChunk(reader)
    if (!chunk.length) return entries
    entries.push(...chunk)
  }
}

async function candidatesFromEntry(entry: FileSystemEntry): Promise<UploadCandidate[]> {
  if (entry.isFile) {
    const file = await readFileEntry(entry as FileSystemFileEntry)
    return [{ file, relativePath: cleanRelativePath(entry.fullPath || file.name) }]
  }
  if (!entry.isDirectory) return []
  const children = await readDirectoryEntries(entry as FileSystemDirectoryEntry)
  return (await Promise.all(children.map(candidatesFromEntry))).flat()
}

export async function candidatesFromDrop(transfer: DataTransfer): Promise<UploadCandidate[]> {
  const entries = Array.from(transfer.items)
    .filter(item => item.kind === 'file')
    .map(item => item.webkitGetAsEntry())
    .filter((entry): entry is FileSystemEntry => entry !== null)
  if (!entries.length) return candidatesFromFiles(transfer.files)
  return (await Promise.all(entries.map(candidatesFromEntry))).flat()
}

export function joinUploadPath(directory: string, relativePath: string): string {
  return [cleanRelativePath(directory), cleanRelativePath(relativePath)].filter(Boolean).join('/')
}
