import { computed, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { BrowserCapabilities, FileEntry } from '../../shared/api/browser'
import { useBrowserSelection } from './useBrowserSelection'

afterEach(() => {
  vi.useRealTimers()
  document.body.replaceChildren()
})

function setup() {
  vi.useFakeTimers()
  const entries = ref<FileEntry[]>(['one.mp4', 'two.mp4'].map(name => ({
    name, path: name, is_dir: false, size: 1, modified: '', mime: 'video/mp4', icon: 'video', locked: false,
  })))
  const panel = document.createElement('section')
  panel.innerHTML = '<div class="file-list-body"><div class="file-row" data-entry-path="one.mp4"><button class="select-box"></button></div><div class="file-row" data-entry-path="two.mp4"></div></div>'
  document.body.append(panel)
  const openEntry = vi.fn()
  const selection = useBrowserSelection({
    entries, visibleEntries: computed(() => entries.value), loading: ref(false),
    capabilities: ref<BrowserCapabilities>({ download: true, upload: true, create_directory: true, rename: true, move_items: true, copy: true, delete: true }),
    filePanel: ref(panel), openEntry,
  })
  const rows = [...panel.querySelectorAll<HTMLElement>('.file-row')]
  function pointer(target: HTMLElement, type: string, pointerType = 'touch', x = 100, y = 120) {
    const event = new PointerEvent(type, { bubbles: true, cancelable: true, pointerType, pointerId: 7, isPrimary: true, button: 0, clientX: x, clientY: y })
    target.dispatchEvent(event)
    return event
  }
  function tap(row = rows[0]!, pointerType = 'touch', x = 100, y = 120) {
    selection.startPointerSelection(pointer(row, 'pointerdown', pointerType, x, y))
    selection.finishPointerSelection(pointer(row, 'pointerup', pointerType, x, y))
    const click = new MouseEvent('click', { cancelable: true, detail: 1 })
    selection.consumeTouchClick(click)
    if (!click.defaultPrevented) selection.handleRowClick(row.dataset.entryPath!)
    return click
  }
  return { ...selection, entries, rows, panel, openEntry, pointer, tap }
}

describe('touch file gestures', () => {
  it.each([['touch', 150], ['touch', 199], ['pen', 150], ['pen', 199]] as const)('opens once after two %s taps %i ms apart, even if a native dblclick also arrives', async (pointerType, delay) => {
    const selection = setup()
    expect(selection.tap(selection.rows[0], pointerType).defaultPrevented).toBe(true)
    expect(selection.selectedPaths.value).toEqual([])
    await vi.advanceTimersByTimeAsync(delay)
    selection.tap(selection.rows[0], pointerType)
    selection.handleRowDoubleClick(selection.entries.value[0]!)
    expect(selection.openEntry).toHaveBeenCalledExactlyOnceWith(selection.entries.value[0])
    await vi.advanceTimersByTimeAsync(400)
    expect(selection.selectedPaths.value).toEqual([])
    expect(selection.contextVisible.value).toBe(false)
    selection.disposeSelection()
  })

  it('applies a single tap at 200 ms and treats later taps as separate selection clicks', async () => {
    const selection = setup()
    selection.tap()
    await vi.advanceTimersByTimeAsync(199)
    expect(selection.selectedPaths.value).toEqual([])
    await vi.advanceTimersByTimeAsync(1)
    expect(selection.selectedPaths.value).toEqual(['one.mp4'])
    selection.tap()
    expect(selection.selectedPaths.value).toEqual(['one.mp4'])
    await vi.advanceTimersByTimeAsync(199)
    expect(selection.selectedPaths.value).toEqual(['one.mp4'])
    await vi.advanceTimersByTimeAsync(1)
    expect(selection.selectedPaths.value).toEqual([])
    expect(selection.openEntry).not.toHaveBeenCalled()
    selection.disposeSelection()
  })

  it('does not combine taps on different entries or distant points into a double-tap', async () => {
    const selection = setup()
    selection.tap()
    await vi.advanceTimersByTimeAsync(100)
    selection.tap(selection.rows[1])
    await vi.advanceTimersByTimeAsync(100)
    selection.tap(selection.rows[1], 'touch', 200, 120)
    await vi.advanceTimersByTimeAsync(201)
    expect(selection.selectedPaths.value).toEqual(['one.mp4'])
    expect(selection.openEntry).not.toHaveBeenCalled()
    selection.disposeSelection()
  })

  it.each(['move', 'cancel', 'navigation'])('resets the double-tap sequence after %s', async reason => {
    const selection = setup()
    selection.tap()
    await vi.advanceTimersByTimeAsync(100)
    if (reason === 'navigation') selection.clearSelection()
    else {
      selection.startPointerSelection(selection.pointer(selection.rows[0]!, 'pointerdown'))
      if (reason === 'move') {
        selection.updatePointerSelection(selection.pointer(selection.rows[0]!, 'pointermove', 'touch', 100, 150))
        selection.finishPointerSelection(selection.pointer(selection.rows[0]!, 'pointerup', 'touch', 100, 150))
      } else selection.cancelPointerSelection()
    }
    selection.tap()
    expect(selection.openEntry).not.toHaveBeenCalled()
    selection.disposeSelection()
  })

  it('does not count a long press as a preview tap', async () => {
    const selection = setup()
    selection.tap()
    await vi.advanceTimersByTimeAsync(100)
    selection.startPointerSelection(selection.pointer(selection.rows[0]!, 'pointerdown'))
    await vi.advanceTimersByTimeAsync(500)
    selection.finishPointerSelection(selection.pointer(selection.rows[0]!, 'pointerup'))
    const release = new MouseEvent('click', { cancelable: true, detail: 1 })
    selection.consumeTouchClick(release)
    expect(release.defaultPrevented).toBe(true)
    expect(selection.contextVisible.value).toBe(true)
    selection.tap()
    await vi.advanceTimersByTimeAsync(201)
    expect(selection.contextVisible.value).toBe(false)
    expect(selection.openEntry).not.toHaveBeenCalled()
    selection.disposeSelection()
  })

  it('does not treat checkbox taps as preview taps', async () => {
    const selection = setup()
    const checkbox = selection.rows[0]!.querySelector<HTMLElement>('.select-box')!
    selection.tap()
    await vi.advanceTimersByTimeAsync(100)
    selection.startPointerSelection(selection.pointer(checkbox, 'pointerdown'))
    selection.finishPointerSelection(selection.pointer(checkbox, 'pointerup'))
    selection.toggleSelection('one.mp4')
    selection.handleRowDoubleClick(selection.entries.value[0]!)
    selection.tap()
    await vi.advanceTimersByTimeAsync(201)
    expect(selection.selectedPaths.value).toEqual([])
    expect(selection.openEntry).not.toHaveBeenCalled()
    selection.disposeSelection()
  })

  it('keeps mouse double-click preview working after touch input', () => {
    const selection = setup()
    selection.tap()
    selection.startPointerSelection(selection.pointer(selection.rows[0]!, 'pointerdown', 'mouse'))
    selection.finishPointerSelection(selection.pointer(selection.rows[0]!, 'pointerup', 'mouse'))
    selection.handleRowDoubleClick(selection.entries.value[0]!)
    expect(selection.openEntry).toHaveBeenCalledExactlyOnceWith(selection.entries.value[0])
    selection.disposeSelection()
  })

  it('preserves an existing multi-selection when double-tapping an entry', async () => {
    const selection = setup()
    selection.toggleSelection('one.mp4')
    selection.toggleSelection('two.mp4')
    selection.tap()
    await vi.advanceTimersByTimeAsync(150)
    selection.tap()
    await vi.advanceTimersByTimeAsync(400)
    expect(selection.selectedPaths.value).toEqual(['one.mp4', 'two.mp4'])
    expect(selection.openEntry).toHaveBeenCalledExactlyOnceWith(selection.entries.value[0])
    selection.disposeSelection()
  })

  it('cancels delayed selection on navigation or unmount', async () => {
    const selection = setup()
    selection.tap()
    selection.clearSelection()
    await vi.advanceTimersByTimeAsync(400)
    expect(selection.selectedPaths.value).toEqual([])
    selection.tap()
    selection.disposeSelection()
    expect(vi.getTimerCount()).toBe(0)
    await vi.advanceTimersByTimeAsync(400)
    expect(selection.selectedPaths.value).toEqual([])
  })
})
