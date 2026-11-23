import { effectScope, nextTick, ref } from 'vue'
import { describe, expect, it } from 'vitest'
import { useScrollReset } from './useScrollReset'

function setup() {
  const host = document.createElement('div')
  const body = document.createElement('div')
  body.className = 'file-list-body'
  host.append(body)
  const panel = ref<HTMLElement | undefined>(host)
  const storage = ref('first')
  const path = ref('')
  const page = ref(1)
  const applied = ref('')
  const query = ref('')
  const scope = effectScope()
  scope.run(() => useScrollReset(panel, [storage, path, applied, page]))
  return { body, storage, path, page, applied, query, scope }
}

describe('scroll reset', () => {
  it('returns to the top when the page changes', async () => {
    const state = setup()
    try {
      state.body.scrollTop = 300
      state.page.value = 2
      await nextTick()
      expect(state.body.scrollTop).toBe(0)
    } finally { state.scope.stop() }
  })

  it('returns to the top when the storage changes', async () => {
    const state = setup()
    try {
      state.body.scrollTop = 300
      state.storage.value = 'second'
      await nextTick()
      expect(state.body.scrollTop).toBe(0)
    } finally { state.scope.stop() }
  })

  it('returns to the top when the directory changes', async () => {
    const state = setup()
    try {
      state.body.scrollTop = 300
      state.path.value = 'photos'
      await nextTick()
      expect(state.body.scrollTop).toBe(0)
    } finally { state.scope.stop() }
  })

  it('keeps the position until a search is actually applied', async () => {
    const state = setup()
    try {
      state.body.scrollTop = 300
      state.query.value = 'holiday'
      await nextTick()
      expect(state.body.scrollTop).toBe(300)
      state.applied.value = 'holiday'
      await nextTick()
      expect(state.body.scrollTop).toBe(0)
    } finally { state.scope.stop() }
  })

  it('keeps the position when nothing that defines the listing changed', async () => {
    const state = setup()
    try {
      state.body.scrollTop = 300
      await nextTick()
      expect(state.body.scrollTop).toBe(300)
    } finally { state.scope.stop() }
  })
})
