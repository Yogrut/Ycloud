import { describe, expect, it } from 'vitest'
import type { BatchItemResult, BatchResponse } from '../../shared/api/browser'
import { buildBatchReport, formatBatchSummary } from './operationFeedback'

describe('operation feedback', () => {
  const outcomes: Array<{ name: string; item: BatchItemResult; expected: 'succeeded' | 'committed' | 'unknown' | 'failed' }> = [
    { name: 'completed', item: { path: 'a', status: 200, code: 'ok', message: 'Completed' }, expected: 'succeeded' },
    { name: 'last non-error status', item: { path: 'a', status: 399, code: 'ok', message: 'Completed' }, expected: 'succeeded' },
    { name: 'first error status', item: { path: 'a', status: 400, code: 'bad_request', message: 'Invalid' }, expected: 'failed' },
    { name: 'permission denied', item: { path: 'a', status: 403, code: 'forbidden', message: 'Denied' }, expected: 'failed' },
    { name: 'legacy committed', item: { path: 'a', status: 409, code: 'operation_committed_pending', message: 'Pending' }, expected: 'committed' },
    { name: 'legacy unknown', item: { path: 'a', status: 409, code: 'operation_result_unknown', message: 'Verify' }, expected: 'unknown' },
    { name: 'committed evidence', item: { path: 'a', status: 503, code: 'service_unavailable', message: 'Pending', operation: { commit: 'committed', cleanup: 'pending', retry: 'do_not_repeat' } }, expected: 'committed' },
    { name: 'unknown evidence', item: { path: 'a', status: 503, code: 'service_unavailable', message: 'Verify', operation: { commit: 'unknown', cleanup: 'unknown', retry: 'verify_first' } }, expected: 'unknown' },
    { name: 'not committed with cleanup pending', item: { path: 'a', status: 503, code: 'service_unavailable', message: 'Failed', operation: { commit: 'not_committed', cleanup: 'pending', retry: 'after_correction' } }, expected: 'failed' },
  ]

  it.each(outcomes)('classifies $name once from the individual result', ({ item, expected }) => {
    const report = buildBatchReport({ success: 99, failed: 99, pending: 99, results: [item] })
    expect(report[expected]).toBe(1)
    expect(report.succeeded + report.committed + report.unknown + report.failed).toBe(1)
    expect(report.attentionItems).toEqual(expected === 'succeeded' ? [] : [item])
  })

  it('keeps detail rows in response order without completed items or duplicate evidence counts', () => {
    const results: BatchItemResult[] = [
      { path: 'z', status: 403, code: 'forbidden', message: 'Denied' },
      { path: 'done', status: 200, code: 'ok', message: 'Completed' },
      { path: 'a', status: 409, code: 'operation_committed_pending', message: 'Pending', operation: { commit: 'committed', cleanup: 'pending', retry: 'do_not_repeat' } },
      { path: 'b', status: 409, code: 'operation_result_unknown', message: 'Verify', operation: { commit: 'unknown', cleanup: 'unknown', retry: 'verify_first' } },
    ]
    const response: BatchResponse = { success: 2, failed: 1, pending: 1, results }
    const before = structuredClone(response)
    const report = buildBatchReport(response)
    expect(report).toEqual({ succeeded: 1, committed: 1, unknown: 1, failed: 1, attentionItems: [results[0], results[2], results[3]] })
    expect(response).toEqual(before)
    expect(report.attentionItems).not.toBe(results)
    expect(formatBatchSummary(report)).toBe('成功 1 项，已提交待收尾 1 项，结果待确认 1 项，失败 1 项。请勿重复执行待确认或已提交项目。')
    expect(formatBatchSummary(report, true)).toBe('1 succeeded, 1 committed with follow-up pending, 1 awaiting confirmation, 1 failed. Do not repeat pending items.')
  })

  it('does not infer individual results from totals for an empty response', () => {
    const report = buildBatchReport({ success: 3, failed: 4, results: [] })
    expect(report).toEqual({ succeeded: 0, committed: 0, unknown: 0, failed: 0, attentionItems: [] })
    expect(formatBatchSummary(report)).toBe('成功 0 项，失败 0 项')
    expect(formatBatchSummary(report, true)).toBe('0 succeeded, 0 failed')
  })

  it('uses the concise summary when results contain only ordinary success and failure', () => {
    const report = buildBatchReport({ success: 2, failed: 1, results: [
      { path: 'a', status: 200, code: 'ok', message: 'Completed' },
      { path: 'b', status: 200, code: 'ok', message: 'Completed' },
      { path: 'c', status: 403, code: 'forbidden', message: 'Denied' },
    ] })
    expect(formatBatchSummary(report)).toBe('成功 2 项，失败 1 项')
    expect(formatBatchSummary(report, true)).toBe('2 succeeded, 1 failed')
  })

  it('separates committed and unknown items from ordinary failures', () => {
    const result = { success: 1, failed: 3, results: [
      { path: 'a', status: 200, code: 'ok', message: 'ok' },
      { path: 'b', status: 409, code: 'operation_committed_pending', message: 'pending' },
      { path: 'c', status: 409, code: 'operation_result_unknown', message: 'unknown' },
      { path: 'd', status: 403, code: 'forbidden', message: 'denied' },
    ] }
    const report = buildBatchReport(result)
    expect(formatBatchSummary(report)).toContain('已提交待收尾 1 项，结果待确认 1 项，失败 1 项')
    expect(formatBatchSummary(report, true)).toContain('Do not repeat pending items')
    expect(buildBatchReport({ ...result, success: 2, failed: 1, pending: 1 })).toEqual(report)
  })
})
