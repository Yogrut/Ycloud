import { effectScope, ref, type EffectScope } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createFolder, renameItem, type FileEntry } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import { useLocale } from '../../shared/i18n'
import { useBrowserEntryEditor } from './useBrowserEntryEditor'

vi.mock('../../shared/api/browser', async importOriginal => ({
  ...await importOriginal<typeof import('../../shared/api/browser')>(),
  createFolder: vi.fn(), renameItem: vi.fn(),
}))

const create = vi.mocked(createFolder)
const rename = vi.mocked(renameItem)
const scopes: EffectScope[] = []
const kinds = ['folder', 'rename'] as const

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail })
  return { promise, resolve, reject }
}

function setup() {
  const scope = effectScope()
  scopes.push(scope)
  const context = {
    path: ref('source'), storageId: ref('primary'), entries: ref<FileEntry[]>([
      { name: 'one.txt', path: 'source/one.txt', is_dir: false, size: 1, modified: '', mime: 'text/plain', icon: 'code', locked: false },
    ]),
    requireCapability: vi.fn().mockReturnValue(true), announce: vi.fn(), refresh: vi.fn().mockResolvedValue(undefined),
  }
  const editor = scope.run(() => useBrowserEntryEditor(context))!
  const open = (kind: typeof kinds[number]) => {
    if (kind === 'folder') editor.openFolderDialog()
    else editor.openRenameDialog('source/one.txt')
  }
  return { context, editor, scope, open }
}

beforeEach(() => {
  useLocale().set('zh-CN')
  create.mockResolvedValue({ success: true })
  rename.mockResolvedValue({ success: true })
})

afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  vi.restoreAllMocks()
  vi.resetAllMocks()
  useLocale().set('zh-CN')
})

describe('browser entry editor', () => {
  it('starts closed without network work and does not submit without an editor', async () => {
    const { editor, context } = setup()
    await editor.submitEntry()
    expect(editor.editorKind.value).toBeNull()
    expect(editor.entryName.value).toBe('')
    expect(editor.entryError.value).toBe('')
    expect(editor.entrySaving.value).toBe(false)
    expect(editor.entryRetryBlocked.value).toBe(false)
    expect(create).not.toHaveBeenCalled()
    expect(rename).not.toHaveBeenCalled()
    expect(context.refresh).not.toHaveBeenCalled()
  })

  it.each(kinds)('requires the matching capability to open %s', kind => {
    const { editor, context, open } = setup()
    context.requireCapability.mockReturnValue(false)
    open(kind)
    expect(context.requireCapability).toHaveBeenCalledExactlyOnceWith(kind === 'folder' ? 'create_directory' : 'rename')
    expect(editor.editorKind.value).toBeNull()
    expect(create).not.toHaveBeenCalled()
    expect(rename).not.toHaveBeenCalled()
  })

  it.each(kinds)('freezes the %s destination and trims only the submitted name', async kind => {
    const { editor, context, open } = setup()
    open(kind)
    expect(editor.entryName.value).toBe(kind === 'folder' ? '' : 'one.txt')
    editor.entryName.value = '  renamed + 文件  '
    context.path.value = 'other'
    context.storageId.value = 'secondary'
    context.entries.value = []
    await editor.submitEntry()
    if (kind === 'folder') {
      expect(create).toHaveBeenCalledExactlyOnceWith('source', 'renamed + 文件', 'primary')
      expect(rename).not.toHaveBeenCalled()
    } else {
      expect(rename).toHaveBeenCalledExactlyOnceWith('source', 'source/one.txt', 'renamed + 文件', 'primary')
      expect(create).not.toHaveBeenCalled()
    }
    expect(context.announce).toHaveBeenCalledExactlyOnceWith(kind === 'folder' ? '文件夹已创建' : '重命名成功', 'success')
    expect(context.refresh).toHaveBeenCalledOnce()
    expect(editor.editorKind.value).toBeNull()
    expect(editor.entryName.value).toBe('')
  })

  it('uses the path basename when the rename target is not in the current entries', () => {
    const { editor } = setup()
    editor.openRenameDialog('elsewhere/space name.txt')
    expect(editor.entryName.value).toBe('space name.txt')
  })

  it.each(kinds)('ignores an empty %s name without closing the editor', async kind => {
    const { editor, open } = setup()
    open(kind)
    editor.entryName.value = '  '
    await editor.submitEntry()
    expect(create).not.toHaveBeenCalled()
    expect(rename).not.toHaveBeenCalled()
    expect(editor.editorKind.value).toBe(kind)
    expect(editor.entrySaving.value).toBe(false)
  })

  it.each(kinds)('does not replace, close or resubmit a pending %s operation', async kind => {
    const pending = deferred<{ success: boolean }>()
    const write = kind === 'folder' ? create : rename
    write.mockReturnValue(pending.promise)
    const { editor, open } = setup()
    open(kind)
    editor.entryName.value = 'first'
    const request = editor.submitEntry()
    editor.openFolderDialog()
    editor.openRenameDialog('source/other.txt')
    editor.closeEntryEditor()
    editor.entryName.value = 'later edit'
    await editor.submitEntry()
    expect(editor.editorKind.value).toBe(kind)
    expect(editor.entrySaving.value).toBe(true)
    expect(write).toHaveBeenCalledOnce()
    expect(write.mock.calls[0]).toContain('first')
    pending.resolve({ success: true })
    await request
    expect(editor.entrySaving.value).toBe(false)
    expect(editor.editorKind.value).toBeNull()
  })

  it.each(kinds)('retains the %s draft after a definite rejection and permits a corrected manual retry', async kind => {
    const write = kind === 'folder' ? create : rename
    write.mockRejectedValueOnce(new ApiError('Denied', 403)).mockResolvedValueOnce({ success: true })
    const { editor, context, open } = setup()
    open(kind)
    editor.entryName.value = 'first'
    await editor.submitEntry()
    expect(editor.entryError.value).toBe('Denied')
    expect(editor.entryName.value).toBe('first')
    expect(editor.editorKind.value).toBe(kind)
    expect(editor.entrySaving.value).toBe(false)
    expect(editor.entryRetryBlocked.value).toBe(false)
    expect(context.announce).not.toHaveBeenCalled()
    expect(context.refresh).not.toHaveBeenCalled()
    editor.entryName.value = 'corrected'
    await editor.submitEntry()
    expect(write).toHaveBeenCalledTimes(2)
    expect(write.mock.lastCall).toContain('corrected')
    expect(editor.entryError.value).toBe('')
  })

  it.each(kinds)('blocks repeated %s confirmation when its result is unknown', async kind => {
    const write = kind === 'folder' ? create : rename
    write.mockRejectedValue(new ApiError('Verify first', 0, 'operation_result_unknown'))
    const { editor, context, open } = setup()
    open(kind)
    editor.entryName.value = 'first'
    await editor.submitEntry()
    editor.entryName.value = 'changed'
    await editor.submitEntry()
    expect(write).toHaveBeenCalledOnce()
    expect(editor.entryRetryBlocked.value).toBe(true)
    expect(editor.entryError.value).toBe('Verify first')
    expect(context.refresh).not.toHaveBeenCalled()
    expect(context.announce).not.toHaveBeenCalled()
  })

  it.each(kinds)('also blocks repeated %s confirmation for explicit committed evidence', async kind => {
    const write = kind === 'folder' ? create : rename
    write.mockRejectedValue(new ApiError('Cleanup pending', 503, 'service_unavailable', undefined, {
      commit: 'committed', cleanup: 'pending', retry: 'do_not_repeat',
    }))
    const { editor, open } = setup()
    open(kind)
    editor.entryName.value = 'first'
    await editor.submitEntry()
    await editor.submitEntry()
    expect(write).toHaveBeenCalledOnce()
    expect(editor.entryRetryBlocked.value).toBe(true)
    expect(editor.entryError.value).toBe('Cleanup pending')
  })

  it('clears draft and feedback on an explicit close or change of editor kind', async () => {
    create.mockRejectedValue(new ApiError('Verify first', 0, 'operation_result_unknown'))
    const { editor } = setup()
    editor.openFolderDialog()
    editor.entryName.value = 'folder'
    await editor.submitEntry()
    editor.closeEntryEditor()
    expect(editor.editorKind.value).toBeNull()
    expect(editor.entryName.value).toBe('')
    expect(editor.entryError.value).toBe('')
    expect(editor.entryRetryBlocked.value).toBe(false)
    editor.openRenameDialog('source/one.txt')
    expect(editor.editorKind.value).toBe('rename')
    expect(editor.entryName.value).toBe('one.txt')
    editor.openFolderDialog()
    expect(editor.editorKind.value).toBe('folder')
    expect(editor.entryName.value).toBe('')
  })

  it.each(kinds)('releases the confirmed %s editor before a slow list refresh and does not disturb a new draft', async kind => {
    const pending = deferred<void>()
    const { editor, context, open } = setup()
    context.refresh.mockReturnValue(pending.promise)
    open(kind)
    editor.entryName.value = 'first'
    const request = editor.submitEntry()
    await Promise.resolve()
    expect(editor.editorKind.value).toBeNull()
    expect(editor.entrySaving.value).toBe(false)
    expect(context.announce).toHaveBeenCalledOnce()
    editor.openFolderDialog()
    editor.entryName.value = 'next'
    pending.resolve()
    await request
    expect(editor.editorKind.value).toBe('folder')
    expect(editor.entryName.value).toBe('next')
    expect(editor.entryError.value).toBe('')
  })

  it.each(kinds)('does not turn confirmed %s success into a write error if list refresh rejects', async kind => {
    const { editor, context, open } = setup()
    context.refresh.mockRejectedValue(new Error('List unavailable'))
    open(kind)
    editor.entryName.value = 'first'
    await editor.submitEntry()
    expect(context.announce.mock.calls).toEqual([
      [kind === 'folder' ? '文件夹已创建' : '重命名成功', 'success'], ['List unavailable'],
    ])
    expect(editor.entryError.value).toBe('')
    expect(editor.editorKind.value).toBeNull()
    await editor.submitEntry()
    expect(kind === 'folder' ? create : rename).toHaveBeenCalledOnce()
  })

  it.each(kinds.flatMap(kind => ['success', 'error'].map(outcome => ({ kind, outcome }))))('ignores $kind $outcome after disposal without follow-up work', async ({ kind, outcome }) => {
    const pending = deferred<{ success: boolean }>()
    const write = kind === 'folder' ? create : rename
    write.mockReturnValue(pending.promise)
    const { editor, context, scope, open } = setup()
    open(kind)
    editor.entryName.value = 'first'
    const request = editor.submitEntry()
    scope.stop()
    if (outcome === 'success') pending.resolve({ success: true })
    else pending.reject(new Error('Offline'))
    await request
    editor.openFolderDialog()
    editor.openRenameDialog('source/one.txt')
    await editor.submitEntry()
    expect(editor.editorKind.value).toBeNull()
    expect(editor.entryName.value).toBe('')
    expect(editor.entryError.value).toBe('')
    expect(editor.entrySaving.value).toBe(false)
    expect(write).toHaveBeenCalledOnce()
    expect(context.announce).not.toHaveBeenCalled()
    expect(context.refresh).not.toHaveBeenCalled()
  })

  it('ignores a refresh rejection after the editor scope ends', async () => {
    const pending = deferred<void>()
    const { editor, context, scope } = setup()
    context.refresh.mockReturnValue(pending.promise)
    editor.openFolderDialog()
    editor.entryName.value = 'first'
    const request = editor.submitEntry()
    await Promise.resolve()
    scope.stop()
    pending.reject(new Error('List unavailable'))
    await request
    expect(context.announce).toHaveBeenCalledExactlyOnceWith('文件夹已创建', 'success')
  })

  it.each(kinds)('keeps the English %s success and fallback errors', async kind => {
    const write = kind === 'folder' ? create : rename
    write.mockRejectedValueOnce('offline').mockResolvedValueOnce({ success: true })
    const { editor, context, open } = setup()
    useLocale().set('en')
    open(kind)
    editor.entryName.value = 'first'
    await editor.submitEntry()
    expect(editor.entryError.value).toBe(kind === 'folder' ? 'Unable to create the folder' : 'Unable to rename the item')
    await editor.submitEntry()
    expect(context.announce).toHaveBeenCalledExactlyOnceWith(kind === 'folder' ? 'Folder created' : 'Renamed', 'success')
  })
})
