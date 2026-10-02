import { computed, onScopeDispose, ref, type Ref } from 'vue'
import { createFolder, renameItem, type BrowserCapabilities, type FileEntry } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import { useLocale } from '../../shared/i18n'

interface EntryEditorContext {
  path: Ref<string>
  storageId: Ref<string>
  entries: Ref<FileEntry[]>
  requireCapability: (action: keyof BrowserCapabilities) => boolean
  announce: (message: string, kind?: 'error' | 'success') => void
  refresh: () => Promise<void>
}

type EntryEdit = { path: string; storageId: string } & (
  { kind: 'folder' } | { kind: 'rename'; target: string }
)

export function useBrowserEntryEditor(context: EntryEditorContext) {
  const locale = useLocale()
  const edit = ref<EntryEdit | null>(null)
  const entryName = ref('')
  const entryError = ref('')
  const entrySaving = ref(false)
  const entryRetryBlocked = ref(false)
  const editorKind = computed(() => edit.value?.kind ?? null)
  let disposed = false

  function openFolderDialog(): void {
    if (disposed || entrySaving.value || !context.requireCapability('create_directory')) return
    edit.value = { kind: 'folder', path: context.path.value, storageId: context.storageId.value }
    entryName.value = ''
    entryError.value = ''
    entryRetryBlocked.value = false
  }

  function openRenameDialog(target: string): void {
    if (disposed || entrySaving.value || !context.requireCapability('rename')) return
    edit.value = { kind: 'rename', path: context.path.value, storageId: context.storageId.value, target }
    entryName.value = context.entries.value.find(entry => entry.path === target)?.name ?? target.split('/').pop() ?? ''
    entryError.value = ''
    entryRetryBlocked.value = false
  }

  function closeEntryEditor(): void {
    if (entrySaving.value) return
    edit.value = null
    entryName.value = ''
    entryError.value = ''
    entryRetryBlocked.value = false
  }

  async function submitEntry(): Promise<void> {
    const request = edit.value
    const name = entryName.value.trim()
    if (disposed || !request || !name || entrySaving.value || entryRetryBlocked.value) return
    entrySaving.value = true
    entryError.value = ''
    try {
      if (request.kind === 'folder') await createFolder(request.path, name, request.storageId)
      else await renameItem(request.path, request.target, name, request.storageId)
    } catch (error) {
      if (!disposed) {
        entryRetryBlocked.value = error instanceof ApiError && error.blocksRetry
        entryError.value = error instanceof Error ? error.message : request.kind === 'folder'
          ? locale.text('创建失败', 'Unable to create the folder')
          : locale.text('重命名失败', 'Unable to rename the item')
      }
      return
    } finally {
      entrySaving.value = false
    }
    if (disposed) return
    closeEntryEditor()
    context.announce(request.kind === 'folder'
      ? locale.text('文件夹已创建', 'Folder created')
      : locale.text('重命名成功', 'Renamed'), 'success')
    // A list refresh cannot change an already confirmed write into a failure.
    try {
      await context.refresh()
    } catch (error) {
      if (!disposed) context.announce(error instanceof Error ? error.message : locale.text('目录加载失败', 'Unable to load this folder'))
    }
  }

  onScopeDispose(() => {
    disposed = true
    edit.value = null
    entryName.value = ''
    entryError.value = ''
    entryRetryBlocked.value = false
    // Do not abort accepted writes; their later results cannot refresh this page.
  })

  return {
    editorKind, entryName, entryError, entrySaving, entryRetryBlocked,
    openFolderDialog, openRenameDialog, closeEntryEditor, submitEntry,
  }
}
