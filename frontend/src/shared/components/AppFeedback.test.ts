import { createApp, h, nextTick, ref } from 'vue'
import { afterEach, expect, it, vi } from 'vitest'
import AppFeedback from './AppFeedback.vue'

afterEach(() => { vi.useRealTimers(); document.body.replaceChildren() })

it('replaces old feedback without resurrecting it, and repeats the same result on a new attempt', async () => {
  vi.useFakeTimers()
  const message = ref('保存失败')
  const success = ref('')
  const revision = ref(0)
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp({ render: () => [
    h(AppFeedback, { message: message.value, revision: revision.value }),
    h(AppFeedback, { message: success.value, kind: 'success' }),
  ] })
  app.mount(host)
  await nextTick()
  expect(document.querySelectorAll('.app-toast')).toHaveLength(1)
  expect(document.querySelector('.app-toast.error')?.textContent).toContain('保存失败')
  success.value = '保存成功'
  await nextTick()
  expect(document.querySelectorAll('.app-toast')).toHaveLength(1)
  expect(document.querySelector('.app-toast.success')?.textContent).toContain('保存成功')
  vi.advanceTimersByTime(6000)
  await nextTick()
  expect(document.querySelector('.app-toast')).toBeNull()
  revision.value++
  await nextTick()
  expect(document.querySelector('.app-toast.error')?.textContent).toContain('保存失败')
  document.querySelector<HTMLButtonElement>('.app-toast button')!.click()
  await nextTick()
  expect(document.querySelector('.app-toast')).toBeNull()
  revision.value++
  await nextTick()
  expect(document.querySelector('.app-toast.error')).not.toBeNull()
  message.value = ''
  await nextTick()
  expect(document.querySelector('.app-toast')).toBeNull()
  app.unmount()
  expect(vi.getTimerCount()).toBe(0)
})
