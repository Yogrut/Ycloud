<script setup lang="ts">
import { computed } from 'vue'
import {
  PhArchive,
  PhArrowCircleDown,
  PhArrowCircleRight,
  PhArrowCircleUp,
  PhArrowsOutCardinal,
  PhCloud,
  PhCopy,
  PhFile,
  PhFileArchive,
  PhFileAudio,
  PhFileCode,
  PhFileDoc,
  PhFileImage,
  PhFilePdf,
  PhFileVideo,
  PhFolder,
  PhFolderOpen,
  PhFolderPlus,
  PhHardDrives,
  PhHouse,
  PhListBullets,
  PhLockKey,
  PhMagnifyingGlass,
  PhMoon,
  PhPencilSimple,
  PhPlugsConnected,
  PhShieldCheck,
  PhSlidersHorizontal,
  PhSun,
  PhTrash,
  PhTranslate,
  PhUserCircle,
  PhUsersThree,
} from '@phosphor-icons/vue'

export type AppIconName =
  | 'account'
  | 'administrator'
  | 'archive'
  | 'cloud'
  | 'copy'
  | 'delete'
  | 'download'
  | 'file'
  | 'file-archive'
  | 'file-audio'
  | 'file-code'
  | 'file-doc'
  | 'file-image'
  | 'file-pdf'
  | 'file-video'
  | 'folder'
  | 'folder-open'
  | 'folder-plus'
  | 'home'
  | 'language'
  | 'limits'
  | 'lock'
  | 'logs'
  | 'moon'
  | 'move'
  | 'rename'
  | 'search'
  | 'sign-out'
  | 'storage'
  | 'sun'
  | 'upload'
  | 'users'
  | 'webdav'

const icons = {
  account: PhUserCircle,
  administrator: PhShieldCheck,
  archive: PhArchive,
  cloud: PhCloud,
  copy: PhCopy,
  delete: PhTrash,
  download: PhArrowCircleDown,
  file: PhFile,
  'file-archive': PhFileArchive,
  'file-audio': PhFileAudio,
  'file-code': PhFileCode,
  'file-doc': PhFileDoc,
  'file-image': PhFileImage,
  'file-pdf': PhFilePdf,
  'file-video': PhFileVideo,
  folder: PhFolder,
  'folder-open': PhFolderOpen,
  'folder-plus': PhFolderPlus,
  home: PhHouse,
  language: PhTranslate,
  limits: PhSlidersHorizontal,
  lock: PhLockKey,
  logs: PhListBullets,
  moon: PhMoon,
  move: PhArrowsOutCardinal,
  rename: PhPencilSimple,
  search: PhMagnifyingGlass,
  'sign-out': PhArrowCircleRight,
  storage: PhHardDrives,
  sun: PhSun,
  upload: PhArrowCircleUp,
  users: PhUsersThree,
  webdav: PhPlugsConnected,
} as const

const props = withDefaults(defineProps<{ name: AppIconName; size?: number | string }>(), { size: 20 })
const icon = computed(() => icons[props.name])
</script>

<template>
  <component
    :is="icon"
    class="ui-icon app-icon"
    :size="size"
    color="currentColor"
    weight="duotone"
    :data-icon="name"
    data-weight="duotone"
    aria-hidden="true"
    focusable="false"
  />
</template>
