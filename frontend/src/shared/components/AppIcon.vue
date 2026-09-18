<script setup lang="ts">
import { computed } from 'vue'
import {
  PhArchive,
  PhArrowClockwise,
  PhArrowCircleDown,
  PhArrowCircleRight,
  PhArrowCircleUp,
  PhArrowsOutCardinal,
  PhCalendarBlank,
  PhCloud,
  PhCheckCircle,
  PhCopy,
  PhFile,
  PhFileText,
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
  PhGearSix,
  PhHardDrives,
  PhHouse,
  PhListBullets,
  PhLockKey,
  PhMagnifyingGlass,
  PhMoon,
  PhPause,
  PhPencilSimple,
  PhPlay,
  PhPlugsConnected,
  PhShieldCheck,
  PhSlidersHorizontal,
  PhStar,
  PhSun,
  PhStop,
  PhTrash,
  PhTranslate,
  PhUserCircle,
  PhUsersThree,
  PhX,
  PhXCircle,
} from '@phosphor-icons/vue'

export type AppIconName =
  | 'account'
  | 'administrator'
  | 'archive'
  | 'calendar'
  | 'cloud'
  | 'status-success'
  | 'status-error'
  | 'close'
  | 'copy'
  | 'delete'
  | 'download'
  | 'file'
  | 'file-text'
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
  | 'pause'
  | 'rename'
  | 'retry'
  | 'resume'
  | 'search'
  | 'settings'
  | 'sign-out'
  | 'storage'
  | 'star'
  | 'stop'
  | 'sun'
  | 'upload'
  | 'users'
  | 'webdav'

const icons = {
  account: PhUserCircle,
  administrator: PhShieldCheck,
  archive: PhArchive,
  calendar: PhCalendarBlank,
  cloud: PhCloud,
  'status-success': PhCheckCircle,
  'status-error': PhXCircle,
  close: PhX,
  copy: PhCopy,
  delete: PhTrash,
  download: PhArrowCircleDown,
  file: PhFile,
  'file-text': PhFileText,
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
  pause: PhPause,
  rename: PhPencilSimple,
  retry: PhArrowClockwise,
  resume: PhPlay,
  search: PhMagnifyingGlass,
  settings: PhGearSix,
  'sign-out': PhArrowCircleRight,
  storage: PhHardDrives,
  star: PhStar,
  stop: PhStop,
  sun: PhSun,
  upload: PhArrowCircleUp,
  users: PhUsersThree,
  webdav: PhPlugsConnected,
} as const

const props = withDefaults(
  defineProps<{ name: AppIconName; size?: number | string; weight?: 'duotone' | 'regular' | 'fill' }>(),
  { size: 20, weight: 'duotone' },
)
const icon = computed(() => icons[props.name])
</script>

<template>
  <component
    :is="icon"
    class="ui-icon app-icon"
    :size="size"
    color="currentColor"
    :weight="weight"
    :data-icon="name"
    :data-weight="weight"
    aria-hidden="true"
    focusable="false"
  />
</template>
