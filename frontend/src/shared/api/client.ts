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

export function errorMetadata(response: Response, body: unknown) {
  const envelope = body as ErrorEnvelope | undefined
  return {
    code: envelope?.error?.code,
    operation: envelope?.error?.operation,
    message: envelope?.error?.message ?? envelope?.message,
    requestId: response.headers.get('x-request-id') ?? undefined,
  }
}
