import { createApp, nextTick, reactive } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import UploadQueueDialog from './UploadQueueDialog.vue'
import type { UploadTask, UploadTaskStatus } from './uploadQueue'

afterEach(() => document.body.replaceChildren())

function task(id: number, status: UploadTaskStatus, error = ''): UploadTask {
  const file = new File(['data'], `${status}.txt`)
  return {
    id,
    file,
    relativePath: file.name,
    storageId: 'primary',
    basePath: '',
    targetPath: file.name,
    status,
    loaded: status === 'succeeded' ? file.size : 0,
    error,
  }
}

function mountUploadDialog(host: HTMLElement, props: Record<string, unknown>) {
  const app = createApp(UploadQueueDialog, props)
  app.mount(host)
  return app
}

describe('UploadQueueDialog', () => {
  it('does not offer direct retry for a result requiring verification', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const pending = { ...task(1, 'failed', '已提交，请核对'), retryBlocked: true }
    const app = mountUploadDialog(host, { tasks: [pending] })
    await nextTick()
    expect(host.textContent).toContain('已提交，请核对')
    expect(host.querySelector('[aria-label="重试该文件"]')).toBeNull()
    app.unmount()
  })

  it('keeps server reconciliation active without offering pause or termination twice', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountUploadDialog(host, {
      tasks: [task(1, 'verifying', '操作仍在服务端执行')],
    })
    await nextTick()

    expect(host.querySelector('.upload-task.is-verifying')?.textContent).toContain('结果确认中')
    expect(host.querySelector('.upload-task.is-verifying')?.textContent).toContain('操作仍在服务端执行')
    expect(host.querySelector('.upload-task.is-verifying button')).toBeNull()
    const clearButton = host.querySelector<HTMLButtonElement>('.upload-batch-actions .danger')
    expect(clearButton?.disabled).toBe(true)
    app.unmount()
  })
  it('keeps every result visible and filters explicit status details', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const clear = vi.fn()
    const app = mountUploadDialog(host, {
      tasks: [
        task(1, 'succeeded'),
        task(2, 'failed', 'network error'),
        task(3, 'paused'),
        task(4, 'cancelled', 'task terminated'),
      ],
      onResume: vi.fn(),
      onClear: clear,
    })
    await nextTick()

    expect(host.querySelectorAll('.upload-task')).toHaveLength(4)
    expect(host.querySelector('.upload-task.is-succeeded')?.textContent).toContain('成功')
    expect(host.querySelector('.upload-task.is-failed')?.textContent).toContain('network error')
    expect(host.querySelector('.upload-task.is-paused')?.textContent).toContain('已暂停')
    expect(host.querySelector('.upload-task.is-cancelled')?.textContent).toContain('已终止')

    const failedFilter = [...host.querySelectorAll<HTMLButtonElement>('.upload-filter-tab')]
      .find(button => button.textContent?.includes('失败/异常'))
    failedFilter?.click()
    await nextTick()
    expect(host.querySelectorAll('.upload-task')).toHaveLength(1)
    expect(host.querySelector('.upload-task')?.textContent).toContain('failed.txt')
    expect(host.querySelector<HTMLElement>('.upload-queue-modal')?.style.getPropertyValue('--upload-modal-height')).toBe('442px')

    host.querySelector<HTMLButtonElement>('.upload-batch-actions .danger')?.click()
    expect(clear).toHaveBeenCalledWith([2])
    app.unmount()
  })

  it('exposes icon actions for each task status without affecting other rows', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const pause = vi.fn()
    const resume = vi.fn()
    const terminate = vi.fn()
    const retry = vi.fn()
    const removeFailed = vi.fn()
    const clear = vi.fn()
    const app = mountUploadDialog(host, {
      tasks: [
        task(1, 'queued'),
        task(2, 'uploading'),
        task(3, 'paused'),
        task(4, 'succeeded'),
        task(5, 'failed', 'network error'),
        task(6, 'cancelled'),
      ],
      onPause: pause,
      onResume: resume,
      onTerminate: terminate,
      onRetry: retry,
      onRemoveFailed: removeFailed,
      onClear: clear,
    })
    await nextTick()

    host.querySelector<HTMLButtonElement>('.upload-task.is-queued button[aria-label="暂停该文件"]')?.click()
    host.querySelector<HTMLButtonElement>('.upload-task.is-uploading button[aria-label="暂停该文件"]')?.click()
    host.querySelector<HTMLButtonElement>('.upload-task.is-uploading button[aria-label="终止该文件"]')?.click()
    host.querySelector<HTMLButtonElement>('.upload-task.is-paused button[aria-label="继续该文件"]')?.click()
    host.querySelector<HTMLButtonElement>('.upload-task.is-succeeded button[aria-label="删除该任务记录"]')?.click()
    host.querySelector<HTMLButtonElement>('.upload-task.is-failed button[aria-label="重试该文件"]')?.click()
    host.querySelector<HTMLButtonElement>('.upload-task.is-failed button[aria-label="删除失败记录"]')?.click()
    host.querySelector<HTMLButtonElement>('.upload-task.is-cancelled button[aria-label="删除该任务记录"]')?.click()

    expect(pause).toHaveBeenNthCalledWith(1, [1])
    expect(pause).toHaveBeenNthCalledWith(2, [2])
    expect(terminate).toHaveBeenCalledWith([2])
    expect(resume).toHaveBeenCalledWith([3])
    expect(clear).toHaveBeenNthCalledWith(1, [4])
    expect(retry).toHaveBeenCalledWith(5)
    expect(removeFailed).toHaveBeenCalledWith(5)
    expect(clear).toHaveBeenNthCalledWith(2, [6])
    app.unmount()
  })

  it('enables filtered pause while a visible file is actively uploading', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const pause = vi.fn()
    const app = mountUploadDialog(host, {
      tasks: [task(1, 'uploading')],
      onPause: pause,
    })
    await nextTick()

    const pauseButton = host.querySelector<HTMLButtonElement>('.upload-batch-actions button[aria-label="暂停当前筛选任务"]')
    expect(pauseButton?.disabled).toBe(false)
    pauseButton?.click()
    expect(pause).toHaveBeenCalledWith([1])
    app.unmount()
  })

  it('shows overall and per-file progress without claiming completion before every file succeeds', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const files = reactive([
      task(1, 'succeeded'),
      { ...task(2, 'uploading'), loaded: 2 },
      task(3, 'queued'),
    ])
    const app = mountUploadDialog(host, { tasks: files })
    await nextTick()

    expect(host.querySelector<HTMLProgressElement>('.upload-overall-progress')?.value).toBe(50)
    expect(host.querySelector('.upload-overall-percent')?.textContent).toBe('50%')
    expect(host.querySelector('.upload-overall-meta')?.textContent).toContain('已完成 1 / 3 个文件')
    expect([...host.querySelectorAll<HTMLProgressElement>('.upload-task progress')].map(progress => progress.value)).toEqual([100, 50, 0])

    files[1]!.loaded = 4
    files[1]!.status = 'verifying'
    await nextTick()
    expect(host.querySelector<HTMLProgressElement>('.upload-task.is-verifying progress')?.value).toBe(99)

    files[1]!.status = 'succeeded'
    files[2]!.status = 'succeeded'
    await nextTick()
    expect(host.querySelector<HTMLProgressElement>('.upload-overall-progress')?.value).toBe(100)
    expect(host.querySelector('.upload-overall-percent')?.textContent).toBe('100%')
    expect(host.querySelector('.upload-overall-meta')?.textContent).toContain('全部上传成功')
    app.unmount()
  })

  it('reports unsuccessful files instead of treating their transferred bytes as completed uploads', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountUploadDialog(host, {
      tasks: [task(1, 'succeeded'), { ...task(2, 'failed', 'network error'), loaded: 4 }],
    })
    await nextTick()
    expect(host.querySelector<HTMLProgressElement>('.upload-overall-progress')?.value).toBe(50)
    expect(host.querySelector<HTMLProgressElement>('.upload-task.is-failed progress')?.value).toBe(0)
    expect(host.querySelector('.upload-overall-meta')?.textContent).toContain('1 个未成功')
    app.unmount()
  })
})
