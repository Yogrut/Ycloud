import { computed, ref } from 'vue'
import type { ComputedRef, Ref } from 'vue'
import type { BrowserCapabilities, FileEntry } from '../../shared/api/browser'

type DragSession = {
  startX: number
  startY: number
  active: boolean
  initialSelection: Set<string>
}

interface BrowserSelectionContext {
  entries: Ref<FileEntry[]>
  visibleEntries: ComputedRef<FileEntry[]>
  loading: Ref<boolean>
  capabilities: Ref<BrowserCapabilities>
  filePanel: Ref<HTMLElement | undefined>
}

const DRAG_THRESHOLD = 6

export function useBrowserSelection(context: BrowserSelectionContext) {
  const selected = ref(new Set<string>())
  const contextVisible = ref(false)
  const contextEntry = ref<FileEntry | null>(null)
  const contextX = ref(0)
  const contextY = ref(0)
  const dragSelecting = ref(false)
  let dragSession: DragSession | null = null
  let suppressRowClick = false
  let suppressRowClickTimer: number | undefined

  const allSelected = computed(() => context.visibleEntries.value.length > 0
    && context.visibleEntries.value.every(entry => selected.value.has(entry.path)))
  const selectedPaths = computed(() => [...selected.value])

  function isMobileLayout(): boolean {
    return window.matchMedia('(max-width: 760px)').matches
  }

  function closeContextMenu(): void {
    contextVisible.value = false
  }

  function syncMobileMenu(paths: Set<string>): void {
    if (!isMobileLayout()) return
    if (!paths.size) {
      contextVisible.value = false
      return
    }
    contextEntry.value = paths.size === 1
      ? context.entries.value.find(entry => paths.has(entry.path)) ?? null
      : null
    contextX.value = 0
    contextY.value = 0
    contextVisible.value = true
  }

  function toggleSelection(entryPath: string): void {
    const next = new Set(selected.value)
    if (next.has(entryPath)) next.delete(entryPath)
    else next.add(entryPath)
    selected.value = next
    syncMobileMenu(next)
  }

  function handleRowClick(entryPath: string): void {
    if (suppressRowClick) {
      suppressRowClick = false
      if (suppressRowClickTimer !== undefined) window.clearTimeout(suppressRowClickTimer)
      return
    }
    toggleSelection(entryPath)
  }

  function toggleSelectAll(): void {
    const next = new Set(selected.value)
    if (allSelected.value) context.visibleEntries.value.forEach(entry => next.delete(entry.path))
    else context.visibleEntries.value.forEach(entry => next.add(entry.path))
    selected.value = next
    syncMobileMenu(next)
  }

  function startDragSelection(event: MouseEvent): void {
    if (event.button !== 0 || isMobileLayout() || context.loading.value || !context.visibleEntries.value.length) return
    const target = event.target as HTMLElement
    const button = target.closest('button')
    if (target.closest('input, select, textarea, a') || (button && !button.classList.contains('select-box'))) return
    // Prevent native text/image dragging before the movement threshold. Row
    // checkboxes still receive an ordinary click when no drag occurs.
    event.preventDefault()
    dragSession = {
      startX: event.clientX,
      startY: event.clientY,
      active: false,
      initialSelection: new Set(selected.value),
    }
    closeContextMenu()
  }

  function updateDragSelection(event: MouseEvent): void {
    const session = dragSession
    const panel = context.filePanel.value
    if (!session || !panel) return
    if ((event.buttons & 1) === 0) {
      cancelDragSelection()
      return
    }
    if (!session.active && Math.hypot(event.clientX - session.startX, event.clientY - session.startY) < DRAG_THRESHOLD) return

    session.active = true
    dragSelecting.value = true
    event.preventDefault()
    window.getSelection()?.removeAllRanges()
    const panelRect = panel.getBoundingClientRect()
    const currentX = Math.min(panelRect.right, Math.max(panelRect.left, event.clientX))
    const currentY = Math.min(panelRect.bottom, Math.max(panelRect.top, event.clientY))
    const left = Math.min(session.startX, currentX)
    const top = Math.min(session.startY, currentY)
    const right = Math.max(session.startX, currentX)
    const bottom = Math.max(session.startY, currentY)
    const next = new Set(session.initialSelection)
    const selecting = currentY >= session.startY
    panel.querySelectorAll<HTMLElement>('.file-row[data-entry-path]').forEach(row => {
      const rowRect = row.getBoundingClientRect()
      const intersects = left < rowRect.right && right > rowRect.left && top < rowRect.bottom && bottom > rowRect.top
      const entryPath = row.dataset.entryPath
      if (!intersects || !entryPath) return
      if (selecting) next.add(entryPath)
      else next.delete(entryPath)
    })
    selected.value = next
  }

  function finishDragSelection(): void {
    if (!dragSession) return
    const wasActive = dragSession.active
    dragSession = null
    dragSelecting.value = false
    if (!wasActive) return
    suppressRowClick = true
    suppressRowClickTimer = window.setTimeout(() => { suppressRowClick = false }, 0)
  }

  function cancelDragSelection(): void {
    dragSession = null
    dragSelecting.value = false
  }

  function openRowMenu(event: MouseEvent, entry: FileEntry): void {
    event.preventDefault()
    if (!selected.value.has(entry.path)) selected.value = new Set([entry.path])
    contextEntry.value = entry
    contextX.value = event.clientX
    contextY.value = event.clientY
    contextVisible.value = true
  }

  function openBackgroundMenu(event: MouseEvent): void {
    if ((event.target as HTMLElement).closest('.file-row')) return
    if (!context.capabilities.value.upload && !context.capabilities.value.create_directory) return
    event.preventDefault()
    selected.value = new Set()
    contextEntry.value = null
    contextX.value = event.clientX
    contextY.value = event.clientY
    contextVisible.value = true
  }

  function clearSelection(): void {
    selected.value = new Set()
    closeContextMenu()
  }

  function handleDocumentClick(): void {
    if (isMobileLayout() && selected.value.size) return
    closeContextMenu()
  }

  function disposeSelection(): void {
    cancelDragSelection()
    if (suppressRowClickTimer !== undefined) window.clearTimeout(suppressRowClickTimer)
  }

  return {
    allSelected,
    cancelDragSelection,
    clearSelection,
    closeContextMenu,
    contextEntry,
    contextVisible,
    contextX,
    contextY,
    disposeSelection,
    dragSelecting,
    finishDragSelection,
    handleDocumentClick,
    handleRowClick,
    openBackgroundMenu,
    openRowMenu,
    selected,
    selectedPaths,
    startDragSelection,
    toggleSelectAll,
    toggleSelection,
    updateDragSelection,
  }
}
