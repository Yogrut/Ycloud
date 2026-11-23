import { watch, type Ref, type WatchSource } from 'vue'

/**
 * Brings the visible list back to the top whenever the listing identity
 * changes (storage, directory, search, sort, page size or page number).
 * Silent refreshes leave those sources untouched, so a reader keeps the
 * position while rows update in the background.
 */
export function useScrollReset(panel: Ref<HTMLElement | undefined>, sources: WatchSource[]): void {
  watch(sources, () => {
    const body = panel.value?.querySelector<HTMLElement>('.file-list-body')
    if (body) body.scrollTop = 0
  }, { flush: 'post' })
}
