import type { BatchItemResult, BatchResponse } from '../../shared/api/browser'

export interface BatchReport {
  succeeded: number
  committed: number
  unknown: number
  failed: number
  attentionItems: BatchItemResult[]
}

export function buildBatchReport(result: BatchResponse): BatchReport {
  const report: BatchReport = { succeeded: 0, committed: 0, unknown: 0, failed: 0, attentionItems: [] }
  // Individual results also work with older responses that omit pending totals.
  for (const item of result.results) {
    if (item.status < 400) {
      report.succeeded++
      continue
    }
    report.attentionItems.push(item)
    if (item.operation?.commit === 'committed' || item.code === 'operation_committed_pending') report.committed++
    else if (item.operation?.commit === 'unknown' || item.code === 'operation_result_unknown') report.unknown++
    else report.failed++
  }
  return report
}

export function formatBatchSummary(report: BatchReport, english = false): string {
  const { succeeded, committed, unknown, failed } = report
  if (!committed && !unknown) return english
    ? `${succeeded} succeeded, ${failed} failed`
    : `成功 ${succeeded} 项，失败 ${failed} 项`
  return english
    ? `${succeeded} succeeded, ${committed} committed with follow-up pending, ${unknown} awaiting confirmation, ${failed} failed. Do not repeat pending items.`
    : `成功 ${succeeded} 项，已提交待收尾 ${committed} 项，结果待确认 ${unknown} 项，失败 ${failed} 项。请勿重复执行待确认或已提交项目。`
}
