import { createApp } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'
import type { FileEntry } from '../../shared/api/browser'
import FileIcon from './FileIcon.vue'

afterEach(() => document.body.replaceChildren())

function mountIcon(entry: FileEntry): HTMLElement {
  const host = document.createElement('div')
  document.body.append(host)
  createApp(FileIcon, { entry }).mount(host)
  return host
}

function entry(overrides: Partial<FileEntry>): FileEntry {
  return {
    name: 'item',
    path: 'item',
    is_dir: false,
    size: 0,
    modified: '',
    mime: 'application/octet-stream',
    icon: 'file',
    locked: false,
    ...overrides,
  }
}

describe('FileIcon', () => {
  it('renders filled SVG icons without exposing font ligature names', () => {
    const folder = mountIcon(entry({ is_dir: true, icon: 'folder' }))
    const folderIcon = folder.querySelector('[data-icon="folder"][data-weight="fill"]')
    expect(folderIcon).not.toBeNull()
    expect(folderIcon?.tagName.toLowerCase()).toBe('svg')
    expect(folder.textContent).not.toContain('folder')

    document.body.replaceChildren()
    const file = mountIcon(entry({}))
    const fileIcon = file.querySelector('[data-icon="file-text"][data-weight="fill"]')
    expect(fileIcon?.tagName.toLowerCase()).toBe('svg')
    expect(file.textContent).not.toContain('description')
  })
})
