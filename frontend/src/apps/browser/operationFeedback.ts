import type { BatchResponse } from '../../shared/api/browser'

export function batchSummary(result: BatchResponse, english = false): string {
  const committed = result.results.filter(item => item.operation?.commit === 'committed' || item.code === 'operation_committed_pending').length
  const unknown = result.results.filter(item => item.operation?.commit === 'unknown' || item.code === 'operation_result_unknown').length
  const failed = result.results.filter(item => item.status >= 400
    && item.operation?.commit !== 'committed' && item.code !== 'operation_committed_pending'
    && item.operation?.commit !== 'unknown' && item.code !== 'operation_result_unknown').length
  const succeeded = result.results.filter(item => item.status < 400).length
  if (!committed && !unknown) return english
    ? `${succeeded} succeeded, ${failed} failed`
    : `成功 ${succeeded} 项，失败 ${failed} 项`
  return english
    ? `${succeeded} succeeded, ${committed} committed with follow-up pending, ${unknown} awaiting confirmation, ${failed} failed. Do not repeat pending items.`
    : `成功 ${succeeded} 项，已提交待收尾 ${committed} 项，结果待确认 ${unknown} 项，失败 ${failed} 项。请勿重复执行待确认或已提交项目。`
}
