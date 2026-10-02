import { useLocale } from '../i18n'
import { ApiError, requestWithDeadline } from './client'

export const TEXT_PREVIEW_BYTES = 2 * 1024 * 1024

export interface TextPreview {
  text: string
  truncated: boolean
}

export function readTextPreview(url: string, signal?: AbortSignal): Promise<TextPreview> {
  return requestWithDeadline(url, { headers: { Range: `bytes=0-${TEXT_PREVIEW_BYTES - 1}` }, signal }, async response => {
    if (!response.ok) {
      void response.body?.cancel().catch(() => undefined)
      const locale = useLocale()
      const message = response.status === 429
        ? locale.text('下载流量已用尽或剩余流量不足，请等待重置或联系管理员', 'Download allowance is exhausted or insufficient. Wait for the reset or contact the administrator.')
        : `${locale.t('preview.failed')} (${response.status})`
      throw new ApiError(message, response.status)
    }

    const range = response.headers.get('Content-Range')?.match(/^bytes\s+\d+-(\d+)\/(\d+)$/i)
    let truncated = range ? Number(range[1]) + 1 < Number(range[2]) : false
    if (!response.body) return { text: '', truncated }

    // Range is only a request: a storage endpoint may still return the whole file.
    const reader = response.body.getReader()
    const bytes = new Uint8Array(TEXT_PREVIEW_BYTES)
    let size = 0
    try {
      while (true) {
        const { done, value } = await reader.read()
        if (done) break
        const accepted = value.subarray(0, TEXT_PREVIEW_BYTES - size)
        bytes.set(accepted, size)
        size += accepted.byteLength
        if (accepted.byteLength < value.byteLength) {
          truncated = true
          break
        }
      }
      return { text: new TextDecoder().decode(bytes.subarray(0, size)), truncated }
    } finally {
      // Stop the unused body without making preview completion wait for cleanup.
      void reader.cancel().catch(() => undefined)
      reader.releaseLock()
    }
  })
}
