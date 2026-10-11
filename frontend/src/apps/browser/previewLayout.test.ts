import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const filePreview = readFileSync('src/apps/browser/FilePreviewDialog.vue', 'utf8')
const gallery = readFileSync('src/apps/browser/GalleryLightbox.vue', 'utf8')
const mediaPlayer = readFileSync('src/shared/components/MediaPlayer.vue', 'utf8')

describe('mobile preview layout contract', () => {
  it('bounds phone and tablet documents without the old almost-fullscreen height', () => {
    expect(filePreview).toContain('@media (max-width: 1024px), (pointer: coarse)')
    expect(filePreview).toContain('.ycloud-preview-overlay { padding: 16px; }')
    expect(filePreview).toContain('.ycloud-preview-dialog.video, .ycloud-preview-dialog.document { height: min(630px, 78dvh); max-height: 78dvh; }')
    expect(filePreview).not.toContain('height: calc(100vh - 16px)')
    expect(filePreview).toMatch(/\.ycloud-preview-dialog\.document \.ycloud-preview-body pre \{[^}]*max-height: none;/)
    expect(filePreview).toMatch(/\.ycloud-preview-body pre \{[^}]*overflow: auto;/)
    expect(filePreview).toMatch(/\.ycloud-preview-head \{[^}]*flex-shrink: 0;/)
  })

  it('shares video and document window dimensions while containing the video without stretching it', () => {
    expect(filePreview).toContain('.ycloud-preview-dialog.video, .ycloud-preview-dialog.document { width: min(1040px, 100%); height: min(630px, calc(100dvh - 48px)); }')
    expect(filePreview).not.toContain('.ycloud-preview-dialog.video { height: auto; }')
    expect(filePreview).toContain('.ycloud-preview-dialog.video :deep(.ycloud-video-stage) { height: 100%; max-height: none; aspect-ratio: auto; }')
    expect(mediaPlayer).toMatch(/\.ycloud-video-stage video \{[^}]*object-fit: contain;/)
    expect(mediaPlayer).toContain('playsinline')
    expect(mediaPlayer).toMatch(/\.ycloud-media-player\.video:fullscreen \.ycloud-video-stage \{[^}]*height: 100%;[^}]*max-height: none;/)
  })

  it('gives images a bounded panel while retaining a full-page modal backdrop', () => {
    expect(gallery).toContain('@media (max-width: 1024px), (pointer: coarse)')
    expect(gallery).toMatch(/\.gallery-lightbox \{[^}]*position: fixed;[^}]*inset: 0;/)
    expect(gallery).toMatch(/\.gallery-lightbox-panel \{[^}]*position: relative;/)
    expect(gallery).toMatch(/\.gallery-lightbox-panel \{[^}]*color: var\(--text\);[^}]*background: var\(--panel-soft\);/)
    expect(gallery).not.toContain('background: #101215')
    expect(gallery).toMatch(/\.gallery-lightbox-close \{[^}]*color: inherit;/)
    expect(gallery).toMatch(/\.gallery-lightbox-toolbar button \{[^}]*color: inherit;/)
    expect(gallery).toContain('width: min(1040px, calc(100% - 32px)); height: min(720px, 78dvh)')
    expect(gallery).toMatch(/\.gallery-lightbox-stage img \{[^}]*max-width: 100%;[^}]*max-height: 100%;[^}]*object-fit: contain;/)
  })

  it('removes dialog limits only in native fullscreen', () => {
    expect(filePreview).toContain('.ycloud-preview-dialog:fullscreen { width: 100%; height: 100%; max-height: none; border: 0; border-radius: 0; }')
    expect(filePreview.lastIndexOf('.ycloud-preview-dialog:fullscreen')).toBeGreaterThan(filePreview.lastIndexOf('@media'))
  })
})
