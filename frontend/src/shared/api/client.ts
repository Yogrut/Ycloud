export interface ErrorEnvelope {
  message?: string
  error?: {
    code?: string
    message?: string
  }
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
    readonly requestId?: string,
  ) {
    super(message)
    this.name = 'ApiError'
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
    message: envelope?.error?.message ?? envelope?.message,
    requestId: response.headers.get('x-request-id') ?? undefined,
  }
}
