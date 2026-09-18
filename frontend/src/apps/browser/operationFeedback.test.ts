import { describe, expect, it } from 'vitest'
import { batchSummary } from './operationFeedback'

describe('operation feedback', () => {
  it('separates committed and unknown items from ordinary failures', () => {
    const result = { success: 1, failed: 3, results: [
      { path: 'a', status: 200, code: 'ok', message: 'ok' },
      { path: 'b', status: 409, code: 'operation_committed_pending', message: 'pending' },
      { path: 'c', status: 409, code: 'operation_result_unknown', message: 'unknown' },
      { path: 'd', status: 403, code: 'forbidden', message: 'denied' },
    ] }
    expect(batchSummary(result)).toContain('已提交待收尾 1 项，结果待核对 1 项，失败 1 项')
    expect(batchSummary(result, true)).toContain('Do not repeat pending items')
  })
})
