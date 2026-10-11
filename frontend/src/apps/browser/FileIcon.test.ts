import { readFileSync, readdirSync } from 'node:fs'
import { createApp, nextTick, ref, type App } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'
import type { FileEntry } from '../../shared/api/browser'
import FileIcon from './FileIcon.vue'
import { fileIconName, materialFileIcons } from './fileIcons'

const apps: App[] = []
afterEach(() => {
  for (const app of apps.splice(0)) app.unmount()
  document.body.replaceChildren()
})

function entry(overrides: Partial<FileEntry> = {}): FileEntry {
  return { name: 'item', path: 'item', is_dir: false, size: 0, modified: '',
    mime: 'application/octet-stream', icon: 'file', locked: false, ...overrides }
}
function mountIcon(value: FileEntry) {
  const host = document.createElement('div')
  document.body.append(host)
  const current = ref(value)
  const app = createApp(FileIcon, { get entry() { return current.value } })
  app.mount(host)
  apps.push(app)
  return { host, current }
}

describe('Material file icons', () => {
  it.each([
    ['notes.txt', 'document'], ['README.md', 'markdown'], ['report.DOCX', 'word'],
    ['table.xlsx', 'table'], ['data.csv', 'table'], ['slides.pptx', 'powerpoint'],
    ['report.pdf', 'pdf'], ['photo.JPG', 'image'], ['camera.ARW', 'image'],
    ['photo.heic', 'image'], ['clip.mp4', 'video'], ['song.flac', 'audio'],
    ['program.exe', 'exe'], ['installer.msi', 'exe'], ['package.deb', 'exe'],
    ['phone.apk', 'android'], ['disk.iso', 'disc'], ['font.woff2', 'font'],
    ['database.sqlite3', 'database'], ['data.json', 'json'], ['data.xml', 'xml'],
    ['config.yaml', 'yaml'], ['Cargo.toml', 'toml'], ['options.ini', 'settings'],
    ['app.js', 'javascript'], ['app.ts', 'typescript'], ['App.vue', 'vue'],
    ['main.py', 'python'], ['main.rs', 'rust'], ['main.c', 'c'], ['main.cpp', 'cpp'],
    ['main.java', 'java'], ['archive.jar', 'jar'], ['main.go', 'go'],
    ['page.html', 'html'], ['style.css', 'css'], ['script.ps1', 'console'],
    ['.gitignore', 'git'], ['Dockerfile', 'docker'], ['book.epub', 'epub'],
    ['message.eml', 'email'], ['payload.bin', 'hex'], ['.env', 'settings'],
  ])('uses the Material icon for %s', (name, icon) => {
    const { host } = mountIcon(entry({ name }))
    const image = host.querySelector<HTMLImageElement>('img')!
    expect(image.dataset.fileIcon).toBe(icon)
    expect(image.getAttribute('src')).toBe(materialFileIcons[icon as keyof typeof materialFileIcons])
    expect(image.alt).toBe('')
    expect(image.getAttribute('draggable')).toBe('false')
    expect(host.querySelector('.file-icon')?.getAttribute('aria-hidden')).toBe('true')
    expect(host.querySelector('.app-icon')).toBeNull()
  })

  it.each(['zip', 'zipx', 'rar', '7z', 'tar', 'gz', 'gzip', 'bz2', 'xz', 'lzma', 'lz4',
    'zst', 'br', 'cab', 'arj', 'lzh', 'lha', 'tgz', 'tbz2', 'txz', 'tzst', '001', '002', '003',
    'tar.gz', 'tar.bz2', 'tar.xz', 'tar.zst', 'part01.rar', '7z.001', 'ZIP'])('recognizes archive .%s', suffix => {
    expect(fileIconName(entry({ name: `backup.${suffix}` }))).toBe('zip')
  })

  it('prioritizes directories even if their names have known file extensions', () => {
    const { host } = mountIcon(entry({ name: 'photos.zip', is_dir: true }))
    expect(host.querySelector('.file-icon.folder img[data-file-icon="folder"]')).not.toBeNull()
  })

  it('uses server categories as a fallback and unknown files use the generic icon', () => {
    expect(fileIconName(entry({ name: 'unknown', icon: 'archive' }))).toBe('zip')
    expect(fileIconName(entry({ name: 'unknown', icon: 'image' }))).toBe('image')
    expect(fileIconName(entry({ name: 'unknown', icon: 'code' }))).toBe('document')
    expect(fileIconName(entry({ name: 'unknown.format' }))).toBe('file')
    expect(fileIconName(entry({ name: 'image' }))).toBe('file')
    expect(fileIconName(entry({ name: '__proto__', icon: '__proto__' }))).toBe('file')
  })

  it('distinguishes a transport-stream video from TypeScript', () => {
    expect(fileIconName(entry({ name: 'recording.ts', icon: 'video' }))).toBe('video')
    expect(fileIconName(entry({ name: 'main.ts', icon: 'code' }))).toBe('typescript')
  })

  it('updates the URL when a reused row changes its file', async () => {
    const { host, current } = mountIcon(entry({ name: 'notes.txt' }))
    current.value.name = 'notes.pdf'
    await nextTick()
    expect(host.querySelector('img')?.getAttribute('data-file-icon')).toBe('pdf')
    expect(host.querySelector('img')?.getAttribute('src')).toBe(materialFileIcons.pdf)
  })

  it('vendors exactly the referenced SVG subset', () => {
    const names = readdirSync('src/assets/material-icons').filter(name => name.endsWith('.svg'))
    expect(names.sort()).toEqual(Object.keys(materialFileIcons).map(name => `${name}.svg`).sort())
  })

  it('uses a solid golden-yellow folder rather than the upstream muted default', () => {
    const source = readFileSync('src/assets/material-icons/folder.svg', 'utf8')
    const svg = new DOMParser().parseFromString(source, 'image/svg+xml')
    expect(svg.querySelector('path')?.getAttribute('fill')).toBe('#f9a825')
  })

  it.each([
    ['image', '#009688'], ['zip', '#f59e0b'], ['document', '#1976d2'],
    ['table', '#16a34a'], ['audio', '#e53935'], ['pdf', '#e53935'],
    ['video', '#f57c00'], ['file', '#607d8b'], ['disc', '#7c3aed'],
  ])('uses the stronger %s palette in the SVG itself', (name, color) => {
    const source = readFileSync(`src/assets/material-icons/${name}.svg`, 'utf8')
    const svg = new DOMParser().parseFromString(source, 'image/svg+xml')
    expect(svg.querySelector(`path[fill="${color}"]`)).not.toBeNull()
  })

  it.each(Object.keys(materialFileIcons))('keeps %s as a self-contained colored SVG', name => {
    const source = readFileSync(`src/assets/material-icons/${name}.svg`, 'utf8')
    const svg = new DOMParser().parseFromString(source, 'image/svg+xml')
    const viewBox = svg.querySelector('svg')?.getAttribute('viewBox')?.split(/\s+/).map(Number) ?? []
    expect(viewBox).toHaveLength(4)
    expect(viewBox.every(Number.isFinite)).toBe(true)
    expect(viewBox[2]).toBeGreaterThan(0)
    expect(viewBox[3]).toBeGreaterThan(0)
    expect(svg.querySelector('parsererror, script, foreignObject, image')).toBeNull()
    expect(source).toMatch(/#[\da-f]{3,8}/i)
    expect(source).not.toMatch(/\son\w+\s*=|(?:href|src)\s*=/i)
  })
})
