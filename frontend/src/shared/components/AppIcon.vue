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
  PhFileCsv,
  PhFileDoc,
  PhFileImage,
  PhFileMd,
  PhFilePdf,
  PhFilePpt,
  PhFileVideo,
  PhFileXls,
  PhFolderOpen,
  PhFolderPlus,
  PhFolderSimple,
  PhGearSix,
  PhHardDrives,
  PhHouse,
  PhImages,
  PhListBullets,
  PhLockKey,
  PhMagnifyingGlass,
  PhMoon,
  PhPause,
  PhPauseCircle,
  PhPencilSimple,
  PhPlay,
  PhPlayCircle,
  PhPlugsConnected,
  PhRepeat,
  PhShieldCheck,
  PhSkipBackCircle,
  PhSkipForwardCircle,
  PhSlidersHorizontal,
  PhSpeakerHigh,
  PhSpeakerX,
  PhStar,
  PhSun,
  PhStop,
  PhTrash,
  PhTranslate,
  PhUserCircle,
  PhUsersThree,
  PhVinylRecord,
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
  | 'file-csv'
  | 'file-doc'
  | 'file-image'
  | 'file-md'
  | 'file-pdf'
  | 'file-ppt'
  | 'file-video'
  | 'file-xls'
  | 'folder-open'
  | 'folder-plus'
  | 'folder-simple'
  | 'home'
  | 'gallery'
  | 'language'
  | 'limits'
  | 'lock'
  | 'list'
  | 'logs'
  | 'moon'
  | 'move'
  | 'pause'
  | 'pause-circle'
  | 'play-circle'
  | 'rename'
  | 'repeat'
  | 'retry'
  | 'resume'
  | 'search'
  | 'settings'
  | 'sign-out'
  | 'skip-back-circle'
  | 'skip-forward-circle'
  | 'storage'
  | 'star'
  | 'stop'
  | 'sun'
  | 'upload'
  | 'users'
  | 'vinyl-record'
  | 'volume'
  | 'volume-off'
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
  'file-csv': PhFileCsv,
  'file-doc': PhFileDoc,
  'file-image': PhFileImage,
  'file-md': PhFileMd,
  'file-pdf': PhFilePdf,
  'file-ppt': PhFilePpt,
  'file-video': PhFileVideo,
  'file-xls': PhFileXls,
  'folder-open': PhFolderOpen,
  'folder-plus': PhFolderPlus,
  'folder-simple': PhFolderSimple,
  home: PhHouse,
  gallery: PhImages,
  language: PhTranslate,
  limits: PhSlidersHorizontal,
  lock: PhLockKey,
  list: PhListBullets,
  logs: PhListBullets,
  moon: PhMoon,
  move: PhArrowsOutCardinal,
  pause: PhPause,
  'pause-circle': PhPauseCircle,
  'play-circle': PhPlayCircle,
  rename: PhPencilSimple,
  repeat: PhRepeat,
  retry: PhArrowClockwise,
  resume: PhPlay,
  search: PhMagnifyingGlass,
  settings: PhGearSix,
  'sign-out': PhArrowCircleRight,
  'skip-back-circle': PhSkipBackCircle,
  'skip-forward-circle': PhSkipForwardCircle,
  storage: PhHardDrives,
  star: PhStar,
  stop: PhStop,
  sun: PhSun,
  upload: PhArrowCircleUp,
  users: PhUsersThree,
  'vinyl-record': PhVinylRecord,
  volume: PhSpeakerHigh,
  'volume-off': PhSpeakerX,
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
