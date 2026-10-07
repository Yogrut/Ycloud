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
import { ApiError, errorMetadata, requestJson, requestWithDeadline, REQUEST_TIMEOUT_MS } from './client'
import type { ErrorEnvelope, OperationOutcome } from './client'

const locale = useLocale()

export async function checkDownload(url: string, signal?: AbortSignal): Promise<void> {
  const response = await requestWithDeadline(url, { method: 'HEAD', cache: 'no-store', signal }, async response => response)
  if (response.ok) return
  const message = response.status === 429
    ? locale.text('下载流量不足或请求过于频繁，请稍后重试或联系管理员', 'Insufficient download allowance or too many requests. Try later or contact the administrator.')
    : response.status === 403
      ? locale.text('没有下载权限', 'Download permission denied')
      : locale.text('暂时无法下载', 'Download is currently unavailable')
  throw new ApiError(message, response.status)
}

export async function isPreviewTrafficExhausted(url: string, signal?: AbortSignal): Promise<boolean> {
  try {
    const response = await requestWithDeadline(url, { method: 'HEAD', cache: 'no-store', signal }, async response => response)
    return response.status === 429
  } catch {
    return false
  }
}

export interface FileListResponse {
  storage_id: string
  storages: BrowserStorage[]
  selection_scope?: string
  empty_reason?: 'unconfigured' | 'forbidden' | 'unavailable' | null
  current_path: string
  parent_path: string | null
  entries: FileEntry[]
  page_start: number
  page_size: number
  next_cursor: string | null
  can_write: boolean
  is_admin?: boolean
  capabilities?: BrowserCapabilities
  max_upload_bytes: number
  max_upload_batch_bytes?: number
  max_upload_batch_entries?: number
  max_archive_bytes: number
  max_archive_entries: number
}

export interface FileListOptions {
  signal?: AbortSignal
  limit?: 10 | 20 | 50 | 100
  cursor?: string
  search?: string
  sort?: 'name' | 'size' | 'time'
  direction?: 'asc' | 'desc'
  gallery?: boolean
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
  requires_login: boolean
  enabled?: boolean
  ready?: boolean
}

export interface ArchivePrepareResponse {
  ticket: string
  total_bytes: number
  file_count: number
  entry_count: number
  max_bytes: number
  max_entries: number
}

export interface UploadBatchItem {
  path: string
  size: number
}

export type UploadBatchItemState = 'pending' | 'in_progress' | 'complete' | 'failed' | 'unknown' | 'cancelled'

export interface UploadBatchStatus {
  ticket: string
  items: Array<{
    path: string
    size: number
    status: UploadBatchItemState
    operation?: OperationOutcome
  }>
}

export interface BatchItemResult {
  operation?: OperationOutcome
  path: string
  status: number
  code: string
  message: string
}

export interface BatchResponse {
  success: number
  failed: number
  pending?: number
  results: BatchItemResult[]
}

export type BatchOperation = 'delete' | 'move' | 'copy'

export async function apiRequest<T>(url: string, options: RequestInit = {}): Promise<T> {
  const { response, body } = await requestJson<T>(url, options)
  if (response.status === 401) {
    window.location.replace(appPath('/'))
    throw new ApiError(locale.t('common.sessionExpired'), response.status, 'unauthorized', response.headers.get('x-request-id') ?? undefined)
  }

  if (!response.ok) {
    const details = errorMetadata(response, body ?? {})
    throw new ApiError(details.message ?? locale.t('common.requestFailed', { status: response.status }), response.status, details.code, details.requestId, details.operation)
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

export function fileApi(path: string, storageId?: string, options: FileListOptions = {}): string {
  const clean = cleanPath(path)
  const parameters = new URLSearchParams()
  if (clean) parameters.set('path', `/${clean}`)
  if (storageId) parameters.set('storage_id', storageId)
  if (options.limit) parameters.set('limit', String(options.limit))
  if (options.cursor) parameters.set('cursor', options.cursor)
  if (options.search) parameters.set('search', options.search)
  if (options.sort) parameters.set('sort', options.sort)
  if (options.direction) parameters.set('direction', options.direction)
  if (options.gallery) parameters.set('gallery', 'true')
  const query = parameters.toString()
  return query ? `/api/files?${query}` : '/api/files'
}

export function listFiles(path: string, storageId?: string, options: FileListOptions = {}): Promise<FileListResponse> {
  return apiRequest<FileListResponse>(fileApi(path, storageId, options), { signal: options.signal })
}

export function listStorages(signal?: AbortSignal): Promise<BrowserStorage[]> {
  return apiRequest<BrowserStorage[]>('/api/storages', { signal })
}

export function calculateDirectorySize(path: string, storageId: string, signal: AbortSignal): Promise<{ size: number }> {
  return apiRequest(actionApi('directory/size', path, storageId), { signal })
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

export function previewUrl(path: string, storageId?: string): string {
  const clean = cleanPath(path)
  return withStorage(`/api/preview?path=${encodeURIComponent(`/${clean}`)}`, storageId)
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

export function prepareUploadBatch(items: UploadBatchItem[], storageId?: string): Promise<{ ticket: string; upload_mode?: 'direct' | 'relay'; items?: Array<{ original_path: string; path: string }> }> {
  return apiRequest(withStorage('/api/upload/prepare', storageId), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ items }),
  })
}

export function getUploadBatchStatus(ticket: string, storageId?: string): Promise<UploadBatchStatus> {
  const query = new URLSearchParams({ batch: ticket })
  if (storageId) query.set('storage_id', storageId)
  return apiRequest(`/api/upload/status?${query.toString()}`)
}

export async function cancelUploadBatch(ticket: string, storageId?: string, paths: string[] = []): Promise<void> {
  await apiRequest(withStorage('/api/upload/cancel', storageId), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(paths.length ? { ticket, paths } : { ticket }),
  })
}

export async function batchOperation(operation: BatchOperation, paths: string[], target = '', storageId?: string): Promise<BatchResponse> {
  const { response, body } = await requestJson<BatchResponse & ErrorEnvelope>(withStorage(`/api/batch/${operation}`, storageId), {
    method: operation === 'move' ? 'PUT' : 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ paths, target: target ? `/${cleanPath(target)}` : '' }),
  })
  if (response.status === 401) {
    window.location.replace(appPath('/'))
    throw new Error(locale.t('common.sessionExpired'))
  }

  if (body && Array.isArray(body.results)) return body
  const details = errorMetadata(response, body)
  if (!response.ok) throw new ApiError(details.message ?? locale.t('common.requestFailed', { status: response.status }), response.status, details.code, details.requestId, details.operation)
  throw new ApiError(locale.text('服务返回了无效的批量操作结果，请先核对结果，不要直接重试', 'The server returned an invalid batch result. Verify before retrying.'), response.status, 'operation_result_unknown')
}

export async function uploadFile(path: string, file: File, onProgress: (loaded: number) => void, storageId?: string, batch?: string, signal?: AbortSignal, direct = false): Promise<void> {
  if (direct) {
    const target = new URL(actionApi('upload', path, storageId), window.location.origin)
    if (batch) target.searchParams.set('batch', batch)
    const { uploadDirectFile } = await import('./directUpload')
    if (await uploadDirectFile(target.search, file, onProgress, signal)) return
  }
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest()
    const abortRequest = () => request.abort()
    let idleTimer: ReturnType<typeof setTimeout> | undefined
    const cleanup = () => {
      clearTimeout(idleTimer)
      signal?.removeEventListener('abort', abortRequest)
    }
    const resetIdleTimer = () => {
      clearTimeout(idleTimer)
      idleTimer = setTimeout(() => {
        cleanup()
        reject(new ApiError(locale.text('上传长时间无响应，系统会自动确认结果或清理临时数据。', 'Upload stalled. The server will check the result or clean temporary data.'), 0, 'operation_result_unknown'))
        request.abort()
      }, REQUEST_TIMEOUT_MS)
    }
    const target = new URL(actionApi('upload', path, storageId), window.location.origin)
    if (batch) target.searchParams.set('batch', batch)
    request.open('PUT', `${target.pathname}${target.search}`)
    request.withCredentials = true
    request.setRequestHeader('Content-Type', 'application/octet-stream')
    request.upload.addEventListener('progress', event => {
      resetIdleTimer()
      if (event.lengthComputable) onProgress(Math.min(file.size, event.loaded))
    })
    request.addEventListener('load', () => {
      if (request.status === 401) {
        cleanup()
        window.location.replace(appPath('/'))
        reject(new Error(locale.t('common.sessionExpired')))
        return
      }
      if (request.status >= 200 && request.status < 300) {
        cleanup()
        onProgress(file.size)
        resolve()
        return
      }
      let message = locale.text(`上传失败 (${request.status})`, `Upload failed (${request.status})`)
      let details: ErrorEnvelope['error']
      try {
        const body = JSON.parse(request.responseText) as ErrorEnvelope
        details = body.error
        message = body.error?.message ?? body.message ?? message
      } catch {
        // Keep the status-based message for non-JSON proxy failures.
      }
      cleanup()
      const unknown = request.status >= 500 && !details?.operation
      if (unknown) message = locale.text('系统正在自动确认上传结果，请稍后查看。', 'The server is automatically checking the upload result; check again shortly.')
      reject(new ApiError(message, request.status, unknown ? 'operation_result_unknown' : details?.code, request.getResponseHeader('x-request-id') ?? undefined, details?.operation))
    })
    request.addEventListener('error', () => {
      cleanup()
      reject(new ApiError(locale.text('连接中断，上传结果尚未确认，请先核对文件，不要直接重试。', 'Connection lost. Check the upload result before retrying.'), 0, 'operation_result_unknown'))
    })
    request.addEventListener('abort', () => {
      cleanup()
      reject(new Error(locale.text('上传已取消', 'Upload cancelled')))
    })
    if (signal?.aborted) {
      reject(new Error(locale.text('上传已取消', 'Upload cancelled')))
      return
    }
    signal?.addEventListener('abort', abortRequest, { once: true })
    resetIdleTimer()
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

export function adminLogin(username: string, password: string, totpCode?: string): Promise<{ success: boolean; message?: string; is_admin: boolean; totp_required?: boolean }> {
  return apiRequest('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password, totp_code: totpCode || undefined }),
  })
}

export function userLogin(username: string, password: string): Promise<{ success: boolean; message?: string; is_admin: boolean }> {
  return apiRequest('/api/user/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password }),
  })
}

export { logoutSession as logout } from './auth'
