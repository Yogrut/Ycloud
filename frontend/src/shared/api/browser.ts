export interface FileEntry {
  name: string
  path: string
  is_dir: boolean
  size: number
  modified: string
  mime: string
  icon: string
  locked: boolean
}

import { appPath } from '../routes'
import { useLocale } from '../i18n'

const locale = useLocale()

export interface FileListResponse {
  storage_id: string
  storages: BrowserStorage[]
  current_path: string
  parent_path: string | null
  entries: FileEntry[]
  truncated: boolean
  can_write: boolean
  is_admin?: boolean
  capabilities?: BrowserCapabilities
  max_upload_bytes: number
  max_archive_bytes: number
  max_archive_entries: number
}

export interface BrowserCapabilities {
  download: boolean
  upload: boolean
  create_directory: boolean
  rename: boolean
  move_items: boolean
  copy: boolean
  delete: boolean
}

export interface BrowserStorage {
  id: string
  name: string
  is_default: boolean
  ready: boolean
}

export interface ArchivePrepareResponse {
  ticket: string
  total_bytes: number
  file_count: number
  entry_count: number
  max_bytes: number
  max_entries: number
}

export interface BatchItemResult {
  path: string
  status: number
  code: string
  message: string
}

export interface BatchResponse {
  success: number
  failed: number
  results: BatchItemResult[]
}

export type BatchOperation = 'delete' | 'move' | 'copy'

interface ErrorEnvelope {
  message?: string
  error?: { message?: string }
}

async function readJson<T>(response: Response): Promise<T | undefined> {
  try {
    return await response.json() as T
  } catch {
    return undefined
  }
}

export async function apiRequest<T>(url: string, options: RequestInit = {}): Promise<T> {
  const response = await fetch(url, { credentials: 'same-origin', ...options })
  if (response.status === 401) {
    window.location.replace(appPath('/'))
    throw new Error(locale.t('common.sessionExpired'))
  }

  const body = await readJson<T & ErrorEnvelope>(response)
  if (!response.ok) {
    throw new Error(body?.error?.message ?? body?.message ?? locale.t('common.requestFailed', { status: response.status }))
  }
  if (body === undefined) throw new Error(locale.t('common.invalidResponse'))
  return body
}

function cleanPath(path: string): string {
  return path.replace(/^\/+|\/+$/g, '')
}

function withStorage(url: string, storageId?: string): string {
  if (!storageId) return url
  return `${url}${url.includes('?') ? '&' : '?'}storage_id=${encodeURIComponent(storageId)}`
}

export function fileApi(path: string, storageId?: string): string {
  const clean = cleanPath(path)
  const url = clean ? `/api/files?path=${encodeURIComponent(`/${clean}`)}` : '/api/files'
  return withStorage(url, storageId)
}

export function listFiles(path: string, storageId?: string): Promise<FileListResponse> {
  return apiRequest<FileListResponse>(fileApi(path, storageId))
}

function actionApi(action: string, path: string, storageId?: string): string {
  const clean = cleanPath(path)
  const url = clean ? `/api/${action}?path=${encodeURIComponent(`/${clean}`)}` : `/api/${action}`
  return withStorage(url, storageId)
}

export function createFolder(path: string, name: string, storageId?: string): Promise<{ success?: boolean; message?: string }> {
  return apiRequest(actionApi('mkdir', path, storageId), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name }),
  })
}

export function downloadUrl(path: string, storageId?: string): string {
  const clean = cleanPath(path)
  return withStorage(`/api/download?path=${encodeURIComponent(`/${clean}`)}`, storageId)
}

export function renameItem(currentPath: string, path: string, newName: string, storageId?: string): Promise<{ success?: boolean }> {
  return apiRequest(actionApi('rename', currentPath, storageId), {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path: `/${cleanPath(path)}`, new_name: newName }),
  })
}

export function prepareArchive(paths: string[], storageId?: string): Promise<ArchivePrepareResponse> {
  return apiRequest(withStorage('/api/archive/prepare', storageId), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ paths: paths.map(path => `/${cleanPath(path)}`) }),
  })
}

export async function batchOperation(operation: BatchOperation, paths: string[], target = '', storageId?: string): Promise<BatchResponse> {
  const response = await fetch(withStorage(`/api/batch/${operation}`, storageId), {
    method: operation === 'move' ? 'PUT' : 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ paths, target: target ? `/${cleanPath(target)}` : '' }),
  })
  if (response.status === 401) {
    window.location.replace(appPath('/'))
    throw new Error(locale.t('common.sessionExpired'))
  }

  const body = await readJson<BatchResponse & ErrorEnvelope>(response)
  if (body && Array.isArray(body.results)) return body
  if (!response.ok) throw new Error(body?.error?.message ?? body?.message ?? locale.t('common.requestFailed', { status: response.status }))
  throw new Error(locale.text('服务返回了无效的批量操作结果', 'The server returned an invalid batch result'))
}

export function uploadFile(path: string, file: File, onProgress: (loaded: number) => void, storageId?: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest()
    request.open('PUT', actionApi('upload', path, storageId))
    request.withCredentials = true
    request.setRequestHeader('Content-Type', 'application/octet-stream')
    request.upload.addEventListener('progress', event => {
      if (event.lengthComputable) onProgress(Math.min(file.size, event.loaded))
    })
    request.addEventListener('load', () => {
      if (request.status === 401) {
        window.location.replace(appPath('/'))
        reject(new Error(locale.t('common.sessionExpired')))
        return
      }
      if (request.status >= 200 && request.status < 300) {
        onProgress(file.size)
        resolve()
        return
      }
      let message = locale.text(`上传失败 (${request.status})`, `Upload failed (${request.status})`)
      try {
        const body = JSON.parse(request.responseText) as ErrorEnvelope
        message = body.error?.message ?? body.message ?? message
      } catch {
        // Keep the status-based message for non-JSON proxy failures.
      }
      reject(new Error(message))
    })
    request.addEventListener('error', () => reject(new Error(locale.t('common.networkInterrupted'))))
    request.addEventListener('abort', () => reject(new Error(locale.text('上传已取消', 'Upload cancelled'))))
    request.send(file)
  })
}

export function unlockFolder(path: string, password: string, storageId?: string): Promise<{ success: boolean; message?: string }> {
  return apiRequest(withStorage('/api/folder/unlock', storageId), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path, password }),
  })
}

export function adminLogin(username: string, password: string): Promise<{ success: boolean; message?: string; is_admin: boolean }> {
  return apiRequest('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password }),
  })
}

export async function logout(): Promise<void> {
  await fetch('/api/logout', { method: 'POST', credentials: 'same-origin' })
}
