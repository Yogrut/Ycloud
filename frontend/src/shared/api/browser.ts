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

export interface FileListResponse {
  current_path: string
  parent_path: string | null
  entries: FileEntry[]
  truncated: boolean
  can_write: boolean
  max_upload_bytes: number
  max_archive_bytes: number
  max_archive_entries: number
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
    throw new Error('登录已失效')
  }

  const body = await readJson<T & ErrorEnvelope>(response)
  if (!response.ok) {
    throw new Error(body?.error?.message ?? body?.message ?? `请求失败 (${response.status})`)
  }
  if (body === undefined) throw new Error('服务返回了无效响应')
  return body
}

function cleanPath(path: string): string {
  return path.replace(/^\/+|\/+$/g, '')
}

export function fileApi(path: string): string {
  const clean = cleanPath(path)
  return clean ? `/api/files?path=${encodeURIComponent(`/${clean}`)}` : '/api/files'
}

export function listFiles(path: string): Promise<FileListResponse> {
  return apiRequest<FileListResponse>(fileApi(path))
}

function actionApi(action: string, path: string): string {
  const clean = cleanPath(path)
  return clean ? `/api/${action}?path=${encodeURIComponent(`/${clean}`)}` : `/api/${action}`
}

export function createFolder(path: string, name: string): Promise<{ success?: boolean; message?: string }> {
  return apiRequest(actionApi('mkdir', path), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name }),
  })
}

export function downloadUrl(path: string): string {
  const clean = cleanPath(path)
  return `/api/download?path=${encodeURIComponent(`/${clean}`)}`
}

export function renameItem(currentPath: string, path: string, newName: string): Promise<{ success?: boolean }> {
  return apiRequest(actionApi('rename', currentPath), {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path: `/${cleanPath(path)}`, new_name: newName }),
  })
}

export function prepareArchive(paths: string[]): Promise<ArchivePrepareResponse> {
  return apiRequest('/api/archive/prepare', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ paths: paths.map(path => `/${cleanPath(path)}`) }),
  })
}

export async function batchOperation(operation: BatchOperation, paths: string[], target = ''): Promise<BatchResponse> {
  const response = await fetch(`/api/batch/${operation}`, {
    method: operation === 'move' ? 'PUT' : 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ paths, target: target ? `/${cleanPath(target)}` : '' }),
  })
  if (response.status === 401) {
    window.location.replace(appPath('/'))
    throw new Error('登录已失效')
  }

  const body = await readJson<BatchResponse & ErrorEnvelope>(response)
  if (body && Array.isArray(body.results)) return body
  if (!response.ok) throw new Error(body?.error?.message ?? body?.message ?? `请求失败 (${response.status})`)
  throw new Error('服务返回了无效的批量操作结果')
}

export function uploadFile(path: string, file: File, onProgress: (loaded: number) => void): Promise<void> {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest()
    request.open('PUT', actionApi('upload', path))
    request.withCredentials = true
    request.setRequestHeader('Content-Type', 'application/octet-stream')
    request.upload.addEventListener('progress', event => {
      if (event.lengthComputable) onProgress(Math.min(file.size, event.loaded))
    })
    request.addEventListener('load', () => {
      if (request.status === 401) {
        window.location.replace(appPath('/'))
        reject(new Error('登录已失效'))
        return
      }
      if (request.status >= 200 && request.status < 300) {
        onProgress(file.size)
        resolve()
        return
      }
      let message = `上传失败 (${request.status})`
      try {
        const body = JSON.parse(request.responseText) as ErrorEnvelope
        message = body.error?.message ?? body.message ?? message
      } catch {
        // Keep the status-based message for non-JSON proxy failures.
      }
      reject(new Error(message))
    })
    request.addEventListener('error', () => reject(new Error('网络连接中断')))
    request.addEventListener('abort', () => reject(new Error('上传已取消')))
    request.send(file)
  })
}

export function unlockFolder(path: string, password: string): Promise<{ success: boolean; message?: string }> {
  return apiRequest('/api/folder/unlock', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path, password }),
  })
}

export function adminLogin(username: string, password: string): Promise<{ success: boolean; message?: string }> {
  return apiRequest('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password }),
  })
}

export async function logout(): Promise<void> {
  await fetch('/api/logout', { method: 'POST', credentials: 'same-origin' })
}
