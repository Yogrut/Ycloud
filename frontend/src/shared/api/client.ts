import { useLocale } from '../i18n'

// Allow the server's default 300-second request budget to finish first.
export const REQUEST_TIMEOUT_MS = 330_000

export interface OperationOutcome {
  commit: 'not_committed' | 'committed' | 'unknown'
  cleanup: 'complete' | 'pending' | 'unknown'
  retry: 'after_correction' | 'do_not_repeat' | 'verify_first'
}

export interface ErrorEnvelope {
  message?: string
  error?: {
    code?: string
    message?: string
    operation?: OperationOutcome
  }
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
    readonly requestId?: string,
    readonly operation?: OperationOutcome,
  ) {
    super(message)
    this.name = 'ApiError'
  }

  get blocksRetry(): boolean {
    return this.operation?.commit === 'committed' || this.operation?.commit === 'unknown'
      || this.code === 'operation_committed_pending' || this.code === 'operation_result_unknown'
  }
}

export async function readJson<T>(response: Response): Promise<T | undefined> {
  try {
    return await response.json() as T
  } catch {
    return undefined
  }
}

export async function requestWithDeadline<T>(url: string, options: RequestInit, consume: (response: Response) => Promise<T>, timeoutMs = REQUEST_TIMEOUT_MS): Promise<T> {
  const locale = useLocale()
  const controller = new AbortController()
  const source = options.signal
  const mutation = !['GET', 'HEAD'].includes((options.method ?? 'GET').toUpperCase())
  const unknown = () => new ApiError(locale.text('连接中断或等待超时，操作结果尚未确认，请先核对结果，不要直接重试。', 'Connection lost or timed out. Verify the operation result before retrying.'), 0, 'operation_result_unknown')
  let timer: ReturnType<typeof setTimeout> | undefined
  let abortSource = () => controller.abort()
  const interrupted = new Promise<never>((_, reject) => {
    abortSource = () => {
      controller.abort(source?.reason)
      reject(source?.reason ?? new DOMException('Aborted', 'AbortError'))
    }
    timer = setTimeout(() => {
      controller.abort()
      reject(mutation ? unknown() : new ApiError(locale.text('请求等待超时，请检查网络后重试', 'Request timed out. Check the network and try again.'), 0, 'request_timeout'))
    }, timeoutMs)
    source?.addEventListener('abort', abortSource, { once: true })
    if (source?.aborted) abortSource()
  })
  try {
    if (source?.aborted) return await interrupted
    return await Promise.race([
      fetch(url, { credentials: 'same-origin', ...options, signal: controller.signal }).then(consume),
      interrupted,
    ])
  } catch (error) {
    if (mutation && error instanceof TypeError && !source?.aborted) throw unknown()
    throw error
  } finally {
    clearTimeout(timer)
    source?.removeEventListener('abort', abortSource)
  }
}

export function requestJson<T>(url: string, options: RequestInit = {}, timeoutMs = REQUEST_TIMEOUT_MS): Promise<{ response: Response; body: T | undefined }> {
  return requestWithDeadline(url, options, async response => {
    const body = await readJson<T>(response)
    const mutation = !['GET', 'HEAD'].includes((options.method ?? 'GET').toUpperCase())
    if (mutation && response.status === 408 && !(body as ErrorEnvelope | undefined)?.error?.operation) {
      throw new ApiError(useLocale().text('操作超时，结果尚未确认', 'The operation timed out; its result is unconfirmed'), response.status, 'operation_result_unknown', response.headers.get('x-request-id') ?? undefined)
    }
    if (body === undefined && mutation && ((response.ok && response.status !== 204) || response.status >= 500)) {
      throw new ApiError(useLocale().text('未收到有效的操作结果，请先核对结果，不要直接重试。', 'No valid operation result was received. Verify the result before retrying.'), response.status, 'operation_result_unknown')
    }
    return { response, body }
  }, timeoutMs)
}

export function errorMetadata(response: Response, body: unknown) {
  const envelope = body as ErrorEnvelope | undefined
  const message = envelope?.error?.message ?? envelope?.message
  const permissionDenied = response.status === 403
    && (!message || (envelope?.error?.code === 'forbidden' && message === 'Access denied'))
  return {
    code: envelope?.error?.code,
    operation: envelope?.error?.operation,
    message: permissionDenied ? useLocale().text('无权限执行此操作', 'Permission denied for this operation') : message,
    requestId: response.headers.get('x-request-id') ?? undefined,
  }
}

export function requireSuccess(body: unknown, status = 200, requestId?: string): asserts body is { success: true } {
  const result = body as (ErrorEnvelope & { success?: boolean }) | undefined
  if (result?.success === true) return
  const locale = useLocale()
  if (result?.success === false) {
    throw new ApiError(result.error?.message ?? result.message ?? locale.text('操作失败', 'Operation failed'), status, result.error?.code, requestId, result.error?.operation)
  }
  throw new ApiError(locale.text('未收到有效的操作回执', 'No valid operation receipt was received'), status, 'operation_result_unknown', requestId)
}
