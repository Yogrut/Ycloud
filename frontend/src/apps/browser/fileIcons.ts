import type { FileEntry } from '../../shared/api/browser'
import androidIcon from '../../assets/material-icons/android.svg?no-inline'
import audioIcon from '../../assets/material-icons/audio.svg?no-inline'
import cIcon from '../../assets/material-icons/c.svg?no-inline'
import consoleIcon from '../../assets/material-icons/console.svg?no-inline'
import cppIcon from '../../assets/material-icons/cpp.svg?no-inline'
import cssIcon from '../../assets/material-icons/css.svg?no-inline'
import databaseIcon from '../../assets/material-icons/database.svg?no-inline'
import discIcon from '../../assets/material-icons/disc.svg?no-inline'
import dockerIcon from '../../assets/material-icons/docker.svg?no-inline'
import documentIcon from '../../assets/material-icons/document.svg?no-inline'
import emailIcon from '../../assets/material-icons/email.svg?no-inline'
import epubIcon from '../../assets/material-icons/epub.svg?no-inline'
import exeIcon from '../../assets/material-icons/exe.svg?no-inline'
import fileIcon from '../../assets/material-icons/file.svg?no-inline'
import folderIcon from '../../assets/material-icons/folder.svg?no-inline'
import fontIcon from '../../assets/material-icons/font.svg?no-inline'
import gitIcon from '../../assets/material-icons/git.svg?no-inline'
import goIcon from '../../assets/material-icons/go.svg?no-inline'
import hexIcon from '../../assets/material-icons/hex.svg?no-inline'
import htmlIcon from '../../assets/material-icons/html.svg?no-inline'
import imageIcon from '../../assets/material-icons/image.svg?no-inline'
import jarIcon from '../../assets/material-icons/jar.svg?no-inline'
import javaIcon from '../../assets/material-icons/java.svg?no-inline'
import javascriptIcon from '../../assets/material-icons/javascript.svg?no-inline'
import jsonIcon from '../../assets/material-icons/json.svg?no-inline'
import markdownIcon from '../../assets/material-icons/markdown.svg?no-inline'
import pdfIcon from '../../assets/material-icons/pdf.svg?no-inline'
import powerpointIcon from '../../assets/material-icons/powerpoint.svg?no-inline'
import pythonIcon from '../../assets/material-icons/python.svg?no-inline'
import rustIcon from '../../assets/material-icons/rust.svg?no-inline'
import settingsIcon from '../../assets/material-icons/settings.svg?no-inline'
import tableIcon from '../../assets/material-icons/table.svg?no-inline'
import tomlIcon from '../../assets/material-icons/toml.svg?no-inline'
import typescriptIcon from '../../assets/material-icons/typescript.svg?no-inline'
import videoIcon from '../../assets/material-icons/video.svg?no-inline'
import vueIcon from '../../assets/material-icons/vue.svg?no-inline'
import wordIcon from '../../assets/material-icons/word.svg?no-inline'
import xmlIcon from '../../assets/material-icons/xml.svg?no-inline'
import yamlIcon from '../../assets/material-icons/yaml.svg?no-inline'
import zipIcon from '../../assets/material-icons/zip.svg?no-inline'

export const materialFileIcons = {
  android: androidIcon,
  audio: audioIcon,
  c: cIcon,
  console: consoleIcon,
  cpp: cppIcon,
  css: cssIcon,
  database: databaseIcon,
  disc: discIcon,
  docker: dockerIcon,
  document: documentIcon,
  email: emailIcon,
  epub: epubIcon,
  exe: exeIcon,
  file: fileIcon,
  folder: folderIcon,
  font: fontIcon,
  git: gitIcon,
  go: goIcon,
  hex: hexIcon,
  html: htmlIcon,
  image: imageIcon,
  jar: jarIcon,
  java: javaIcon,
  javascript: javascriptIcon,
  json: jsonIcon,
  markdown: markdownIcon,
  pdf: pdfIcon,
  powerpoint: powerpointIcon,
  python: pythonIcon,
  rust: rustIcon,
  settings: settingsIcon,
  table: tableIcon,
  toml: tomlIcon,
  typescript: typescriptIcon,
  video: videoIcon,
  vue: vueIcon,
  word: wordIcon,
  xml: xmlIcon,
  yaml: yamlIcon,
  zip: zipIcon,
}

type MaterialFileIcon = keyof typeof materialFileIcons

// Appearance only: these extensions do not grant browser preview support.
const extensionGroups = {
  zip: ['zip', 'zipx', 'rar', '7z', 'tar', 'gz', 'gzip', 'bz', 'bz2', 'bzip2', 'xz', 'z', 'lz', 'lzma', 'lz4', 'zst', 'zstd', 'br', 'cab', 'arj', 'lzh', 'lha', 'tgz', 'tbz', 'tbz2', 'txz', 'tlz', 'tzst', '001', '002', '003'],
  image: ['jpg', 'jpeg', 'png', 'gif', 'webp', 'avif', 'bmp', 'ico', 'svg', 'tif', 'tiff', 'heic', 'heif', 'jxl', 'raw', 'arw', 'cr2', 'cr3', 'nef', 'dng', 'orf', 'rw2', 'raf', 'psd'],
  video: ['mp4', 'm4v', 'mkv', 'webm', 'mov', 'avi', 'wmv', 'flv', 'mpg', 'mpeg', 'm2ts', 'vob', 'ogv', 'rm', 'rmvb'],
  audio: ['mp3', 'flac', 'wav', 'm4a', 'aac', 'ogg', 'opus', 'wma', 'aif', 'aiff', 'alac', 'ape', 'amr', 'mid', 'midi', 'm3u', 'm3u8'],
  pdf: ['pdf'],
  word: ['doc', 'docx', 'docm', 'odt', 'rtf'],
  table: ['xls', 'xlsx', 'xlsm', 'xlsb', 'csv', 'tsv', 'ods'],
  powerpoint: ['ppt', 'pptx', 'pptm', 'odp'],
  document: ['txt', 'text', 'log', 'srt', 'vtt', 'ass', 'ssa', 'nfo'],
  markdown: ['md', 'markdown', 'mdown', 'rst'],
  epub: ['epub', 'mobi', 'azw', 'azw3'],
  exe: ['exe', 'msi', 'msix', 'appx', 'appimage', 'deb', 'rpm', 'dmg', 'pkg'],
  android: ['apk', 'apks', 'xapk', 'aab'],
  disc: ['iso', 'img', 'nrg', 'vhd', 'vhdx', 'vdi', 'vmdk'],
  hex: ['bin', 'dat', 'hex'],
  font: ['ttf', 'otf', 'woff', 'woff2', 'eot'],
  database: ['db', 'sqlite', 'sqlite3', 'sql', 'mdb', 'accdb'],
  json: ['json', 'jsonc', 'json5', 'jsonl', 'ndjson'],
  xml: ['xml', 'xsd', 'xsl', 'xslt', 'plist'],
  yaml: ['yml', 'yaml'],
  toml: ['toml'],
  settings: ['ini', 'cfg', 'conf', 'config', 'properties', 'env'],
  javascript: ['js', 'mjs', 'cjs', 'jsx'],
  typescript: ['ts', 'mts', 'cts', 'tsx'],
  vue: ['vue'],
  python: ['py', 'pyw', 'pyi'],
  rust: ['rs'],
  c: ['c', 'h'],
  cpp: ['cpp', 'cc', 'cxx', 'hpp', 'hxx'],
  java: ['java', 'class'],
  jar: ['jar', 'war', 'ear'],
  go: ['go'],
  html: ['html', 'htm', 'xhtml'],
  css: ['css', 'scss', 'sass', 'less'],
  console: ['sh', 'bash', 'zsh', 'fish', 'ps1', 'bat', 'cmd'],
  git: ['gitignore', 'gitattributes', 'gitmodules'],
  email: ['eml', 'msg'],
} satisfies Partial<Record<MaterialFileIcon, string[]>>

const extensions = new Map<string, MaterialFileIcon>(Object.entries(extensionGroups)
  .flatMap(([icon, suffixes]) => suffixes.map(suffix => [suffix, icon as MaterialFileIcon] as const)))
const filenames = new Map<string, MaterialFileIcon>([
  ['dockerfile', 'docker'], ['containerfile', 'docker'],
  ['makefile', 'console'], ['.env', 'settings'],
])
const kinds = new Map<string, MaterialFileIcon>([
  ['image', 'image'], ['video', 'video'], ['audio', 'audio'],
  ['archive', 'zip'], ['pdf', 'pdf'], ['code', 'document'], ['doc', 'document'],
])

export function fileIconName(entry: Pick<FileEntry, 'name' | 'is_dir' | 'icon'>): MaterialFileIcon {
  if (entry.is_dir) return 'folder'
  const name = entry.name.toLowerCase()
  const named = filenames.get(name)
  if (named) return named
  const dot = name.lastIndexOf('.')
  const extension = dot >= 0 ? name.slice(dot + 1) : ''
  // .ts is ambiguous: the server's media classification distinguishes MPEG-TS.
  if (extension === 'ts' && entry.icon === 'video') return 'video'
  return extensions.get(extension) ?? kinds.get(entry.icon) ?? 'file'
}
