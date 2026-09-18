import type { BatchResponse } from '../../shared/api/browser'

export function batchSummary(result: BatchResponse, english = false): string {
  const committed = result.results.filter(item => item.operation?.commit === 'committed' || item.code === 'operation_committed_pending').length
  const unknown = result.results.filter(item => item.operation?.commit === 'unknown' || item.code === 'operation_result_unknown').length
  const failed = Math.max(0, result.failed - committed - unknown)
  if (!committed && !unknown) return english
    ? `${result.success} succeeded, ${failed} failed`
    : `成功 ${result.success} 项，失败 ${failed} 项`
  return english
    ? `${result.success} succeeded, ${committed} committed with follow-up pending, ${unknown} need verification, ${failed} failed. Do not repeat pending items.`
    : `成功 ${result.success} 项，已提交待收尾 ${committed} 项，结果待核对 ${unknown} 项，失败 ${failed} 项。请勿重复执行待核对或已提交项目。`
}
