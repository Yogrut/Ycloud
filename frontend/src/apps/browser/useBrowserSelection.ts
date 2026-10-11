import { computed, ref } from 'vue'
import type { ComputedRef, Ref } from 'vue'
import type { BrowserCapabilities, FileEntry } from '../../shared/api/browser'

type DragSession = {
  pointerId: number
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
  openEntry: (entry: FileEntry) => void
}

const DRAG_THRESHOLD = 6
const LONG_PRESS_DELAY = 500
const TOUCH_MOVE_THRESHOLD = 10
const DOUBLE_TAP_DELAY = 200
const DOUBLE_TAP_DISTANCE = 24

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
  let touchPress: { event: PointerEvent; entry: FileEntry | null; timer: number } | null = null
  let suppressTouchClick = false
  let lastPointerType = 'mouse'
  let pendingTouchTap: { path: string; time: number; x: number; y: number; timer: number } | null = null

  const allSelected = computed(() => context.visibleEntries.value.length > 0
    && context.visibleEntries.value.every(entry => selected.value.has(entry.path)))
  const selectedPaths = computed(() => [...selected.value])

  function cancelTouchPress(): void {
    if (touchPress) window.clearTimeout(touchPress.timer)
    touchPress = null
  }

  function cancelTouchTap(): void {
    if (pendingTouchTap) window.clearTimeout(pendingTouchTap.timer)
    pendingTouchTap = null
  }

  function closeContextMenu(): void {
    cancelTouchPress()
    cancelTouchTap()
    contextVisible.value = false
  }

  function consumeTouchClick(event: MouseEvent): void {
    if (!suppressTouchClick || event.detail === 0) return
    suppressTouchClick = false
    event.preventDefault()
    event.stopImmediatePropagation()
  }

  function toggleSelection(entryPath: string): void {
    const next = new Set(selected.value)
    if (next.has(entryPath)) next.delete(entryPath)
    else next.add(entryPath)
    selected.value = next
    cancelTouchTap()
    contextVisible.value = false
  }

  function handleRowClick(entryPath: string): void {
    if (suppressRowClick) {
      suppressRowClick = false
      if (suppressRowClickTimer !== undefined) window.clearTimeout(suppressRowClickTimer)
      return
    }
    toggleSelection(entryPath)
  }

  function handleRowDoubleClick(entry: FileEntry): void {
    // Touch taps are handled at pointerup, without relying on a synthetic dblclick.
    if (lastPointerType === 'touch' || lastPointerType === 'pen') return
    closeContextMenu()
    context.openEntry(entry)
  }

  function toggleSelectAll(): void {
    const next = new Set(selected.value)
    if (allSelected.value) context.visibleEntries.value.forEach(entry => next.delete(entry.path))
    else context.visibleEntries.value.forEach(entry => next.add(entry.path))
    selected.value = next
    closeContextMenu()
  }

  function startPointerSelection(event: PointerEvent): void {
    cancelTouchPress()
    suppressTouchClick = false
    lastPointerType = event.pointerType
    const panel = context.filePanel.value
    const target = event.target
    if (!(target instanceof Element) || !panel?.contains(target) || context.loading.value || event.button !== 0) {
      cancelTouchTap()
      return
    }
    const button = target.closest('button')
    if (event.pointerType === 'touch' || event.pointerType === 'pen') {
      if (!event.isPrimary || !target.closest('.file-list-body')
        || target.closest('input, select, textarea, a') || (button && !button.classList.contains('gallery-card-open'))) {
        cancelTouchTap()
        return
      }
      const row = target.closest<HTMLElement>('[data-entry-path]')
      const entry = row ? context.entries.value.find(item => item.path === row.dataset.entryPath) : null
      if (row && !entry) return
      if (!target.closest('.file-row')) cancelTouchTap()
      touchPress = {
        event,
        entry: entry ?? null,
        timer: window.setTimeout(() => {
          if (!panel.contains(target) || context.loading.value) return
          if (entry) openRowMenu(event, entry)
          else openBackgroundMenu(event)
        }, LONG_PRESS_DELAY),
      }
      return
    }
    cancelTouchTap()
    if (event.pointerType !== 'mouse' || panel.classList.contains('gallery-mode') || !context.visibleEntries.value.length) return
    if (target.closest('input, select, textarea, a') || (button && !button.classList.contains('select-box'))) return
    // Prevent native text/image dragging before the movement threshold. Row
    // checkboxes still receive an ordinary click when no drag occurs.
    event.preventDefault()
    dragSession = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      active: false,
      initialSelection: new Set(selected.value),
    }
    closeContextMenu()
  }

  function updatePointerSelection(event: PointerEvent): void {
    if (touchPress?.event.pointerId === event.pointerId
      && Math.hypot(event.clientX - touchPress.event.clientX, event.clientY - touchPress.event.clientY) >= TOUCH_MOVE_THRESHOLD) {
      cancelTouchPress()
      cancelTouchTap()
    }
    const session = dragSession
    const panel = context.filePanel.value
    if (!session || !panel || session.pointerId !== event.pointerId) return
    if ((event.buttons & 1) === 0) {
      cancelPointerSelection()
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

  function finishPointerSelection(event: PointerEvent): void {
    const press = touchPress
    if (press?.event.pointerId === event.pointerId) {
      cancelTouchPress()
      const target = press.event.target
      if (press.entry && target instanceof Element && target.closest('.file-row')
        && context.filePanel.value?.contains(target) && !context.loading.value
        && Math.hypot(event.clientX - press.event.clientX, event.clientY - press.event.clientY) < TOUCH_MOVE_THRESHOLD) {
        const previous = pendingTouchTap
        const time = performance.now()
        if (previous?.path === press.entry.path && time - previous.time <= DOUBLE_TAP_DELAY
          && Math.hypot(event.clientX - previous.x, event.clientY - previous.y) <= DOUBLE_TAP_DISTANCE) {
          closeContextMenu()
          context.openEntry(press.entry)
        } else {
          // A tap on another file is a separate single click, not part of this double-tap.
          cancelTouchTap()
          if (previous) toggleSelection(previous.path)
          contextVisible.value = false
          const path = press.entry.path
          pendingTouchTap = {
            path, time, x: event.clientX, y: event.clientY,
            timer: window.setTimeout(() => toggleSelection(path), DOUBLE_TAP_DELAY),
          }
        }
        // The tap is scheduled or activated; do not also process the compatibility click.
        suppressTouchClick = true
      }
    }
    if (!dragSession) return
    if (dragSession.pointerId !== event.pointerId) return
    const wasActive = dragSession.active
    dragSession = null
    dragSelecting.value = false
    if (!wasActive) return
    suppressRowClick = true
    suppressRowClickTimer = window.setTimeout(() => { suppressRowClick = false }, 0)
  }

  function cancelPointerSelection(): void {
    cancelTouchPress()
    cancelTouchTap()
    dragSession = null
    dragSelecting.value = false
  }

  function openRowMenu(event: MouseEvent, entry: FileEntry): void {
    event.preventDefault()
    cancelTouchTap()
    if (touchPress || (event as PointerEvent).pointerType === 'touch' || (event as PointerEvent).pointerType === 'pen') suppressTouchClick = true
    cancelTouchPress()
    if (!selected.value.has(entry.path)) selected.value = new Set([entry.path])
    contextEntry.value = entry
    contextX.value = event.clientX
    contextY.value = event.clientY
    contextVisible.value = true
  }

  function openBackgroundMenu(event: MouseEvent): void {
    if ((event.target as HTMLElement).closest('.file-row, .gallery-card')) return
    if (!context.capabilities.value.upload && !context.capabilities.value.create_directory) return
    event.preventDefault()
    cancelTouchTap()
    if (touchPress || (event as PointerEvent).pointerType === 'touch' || (event as PointerEvent).pointerType === 'pen') suppressTouchClick = true
    cancelTouchPress()
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
    closeContextMenu()
  }

  function disposeSelection(): void {
    cancelPointerSelection()
    suppressTouchClick = false
    if (suppressRowClickTimer !== undefined) window.clearTimeout(suppressRowClickTimer)
  }

  return {
    allSelected,
    cancelPointerSelection,
    clearSelection,
    closeContextMenu,
    contextEntry,
    contextVisible,
    contextX,
    contextY,
    consumeTouchClick,
    disposeSelection,
    dragSelecting,
    finishPointerSelection,
    handleDocumentClick,
    handleRowClick,
    handleRowDoubleClick,
    openBackgroundMenu,
    openRowMenu,
    selected,
    selectedPaths,
    startPointerSelection,
    toggleSelectAll,
    toggleSelection,
    updatePointerSelection,
  }
}
