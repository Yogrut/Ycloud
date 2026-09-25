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
  it('keeps folders filled but gives files lighter duotone SVGs', () => {
    const folder = mountIcon(entry({ is_dir: true, icon: 'folder' }))
    const folderIcon = folder.querySelector('[data-icon="folder-simple"][data-weight="fill"]')
    expect(folderIcon).not.toBeNull()
    expect(folderIcon?.tagName.toLowerCase()).toBe('svg')
    expect(folder.textContent).not.toContain('folder')

    document.body.replaceChildren()
    const file = mountIcon(entry({}))
    const fileIcon = file.querySelector('[data-icon="file"][data-weight="duotone"]')
    expect(fileIcon?.tagName.toLowerCase()).toBe('svg')
    expect(file.textContent).not.toContain('description')
  })

  it.each([
    ['notes.txt', 'code', 'file-text'],
    ['README.md', 'code', 'file-md'],
    ['report.docx', 'doc', 'file-doc'],
    ['table.xlsx', 'doc', 'file-xls'],
    ['data.csv', 'doc', 'file-csv'],
    ['slides.pptx', 'doc', 'file-ppt'],
    ['photo.jpg', 'image', 'file-image'],
    ['song.flac', 'audio', 'file-audio'],
  ])('uses the matching Phosphor file icon for %s', (name, kind, iconName) => {
    const host = mountIcon(entry({ name, path: name, icon: kind }))
    expect(host.querySelector(`[data-icon="${iconName}"][data-weight="duotone"]`)).not.toBeNull()
  })

  it.each([
    ['folder', true, 'folder', 'tone-folder'],
    ['clip.mp4', false, 'video', 'tone-visual'],
    ['photo.jpg', false, 'image', 'tone-visual'],
    ['song.flac', false, 'audio', 'tone-audio'],
    ['notes.md', false, 'code', 'tone-document'],
    ['report.pdf', false, 'pdf', 'tone-document'],
    ['archive.zip', false, 'archive', 'tone-utility'],
    ['program.exe', false, 'file', 'tone-utility'],
    ['other.bin', false, 'file', 'tone-utility'],
  ])('assigns %s to its color group', (name, isDir, iconKind, tone) => {
    const host = mountIcon(entry({ name, path: name, is_dir: isDir, icon: iconKind }))
    expect(host.querySelector('.file-icon')?.classList.contains(tone)).toBe(true)
  })
})
