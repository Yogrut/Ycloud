export type PreviewKind = 'image' | 'video' | 'audio' | 'pdf' | 'text' | 'unsupported'

const imageExtensions = new Set(['jpg', 'jpeg', 'png', 'gif', 'webp', 'avif'])
const videoExtensions = new Set(['mp4', 'webm'])
const audioExtensions = new Set(['mp3', 'm4a', 'wav', 'flac'])
const textExtensions = new Set([
  'txt', 'md', 'markdown', 'json', 'csv', 'xml', 'yaml', 'yml', 'toml', 'log',
  'rs', 'py', 'js', 'ts', 'go', 'java', 'c', 'cpp', 'h', 'html', 'css', 'sh',
  'sql', 'vue', 'svelte', 'rb', 'php', 'swift', 'kt', 'cs', 'lua',
])

export function previewKind(name: string): PreviewKind {
  const dot = name.lastIndexOf('.')
  const extension = dot < 0 ? '' : name.slice(dot + 1).toLowerCase()
  if (imageExtensions.has(extension)) return 'image'
  if (videoExtensions.has(extension)) return 'video'
  if (audioExtensions.has(extension)) return 'audio'
  if (extension === 'pdf') return 'pdf'
  if (textExtensions.has(extension)) return 'text'
  return 'unsupported'
}
