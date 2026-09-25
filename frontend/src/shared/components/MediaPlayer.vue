<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { PhArrowsOut, PhPause, PhPlay, PhSkipBack, PhSkipForward, PhSpeakerHigh, PhSpeakerX } from '@phosphor-icons/vue'
import AppIcon from './AppIcon.vue'
import { useLocale } from '../i18n'

const props = defineProps<{ src: string; name: string; kind: 'video' | 'audio'; hasPrevious?: boolean; hasNext?: boolean; showClose?: boolean; autoplay?: boolean }>()
const emit = defineEmits<{ error: []; previous: []; next: []; close: [] }>()
const locale = useLocale()
const player = ref<HTMLElement | null>(null)
const media = ref<HTMLMediaElement | null>(null)
const volumeControl = ref<HTMLElement | null>(null)
const volumeButton = ref<HTMLButtonElement | null>(null)
const volumeOpen = ref(false)
const playing = ref(false)
const looping = ref(false)
const muted = ref(false)
const volume = ref(1)
const currentTime = ref(0)
const duration = ref(0)

function syncProgress(): void {
  const element = media.value
  if (!element) return
  currentTime.value = element.currentTime || 0
  duration.value = Number.isFinite(element.duration) ? element.duration : 0
  volume.value = element.volume
  muted.value = element.muted
}

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00'
  const minutes = Math.floor(seconds / 60)
  const remainder = Math.floor(seconds % 60).toString().padStart(2, '0')
  return `${minutes}:${remainder}`
}

async function togglePlayback(): Promise<void> {
  const element = media.value
  if (!element) return
  if (!element.paused) { element.pause(); return }
  try { await element.play() } catch { emit('error') }
}

function skip(seconds: number): void {
  if (!media.value || !duration.value) return
  media.value.currentTime = Math.max(0, Math.min(duration.value, media.value.currentTime + seconds))
  syncProgress()
}

function seek(event: Event): void {
  if (!media.value) return
  media.value.currentTime = Number((event.target as HTMLInputElement).value)
  syncProgress()
}

function changeVolume(event: Event): void {
  if (!media.value) return
  media.value.volume = Number((event.target as HTMLInputElement).value)
  media.value.muted = media.value.volume === 0
  syncProgress()
}

function closeVolumeOnOutsidePointer(event: PointerEvent): void {
  if (volumeOpen.value && !volumeControl.value?.contains(event.target as Node)) volumeOpen.value = false
}

function closeVolume(): void {
  volumeOpen.value = false
  volumeButton.value?.focus()
}

function toggleMute(): void {
  if (!media.value) return
  media.value.muted = !media.value.muted
  syncProgress()
}

function toggleLoop(): void {
  if (!media.value) return
  media.value.loop = !media.value.loop
  looping.value = media.value.loop
}

async function toggleFullscreen(): Promise<void> {
  if (document.fullscreenElement) await document.exitFullscreen()
  else await player.value?.requestFullscreen?.()
}

function onEnded(): void {
  playing.value = false
  if (props.kind === 'audio' && props.hasNext) emit('next')
}

onMounted(() => {
  if (props.kind === 'audio') document.addEventListener('pointerdown', closeVolumeOnOutsidePointer)
  if (props.autoplay) void media.value?.play().catch(() => undefined)
})
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', closeVolumeOnOutsidePointer)
  media.value?.pause()
})
</script>

<template>
  <section ref="player" class="ycloud-media-player" :class="kind" :aria-label="name">
    <div v-if="kind === 'video'" class="ycloud-video-stage">
      <video ref="media" :src="src" preload="metadata" playsinline @click="togglePlayback" @loadedmetadata="syncProgress" @durationchange="syncProgress" @timeupdate="syncProgress" @volumechange="syncProgress" @play="playing = true" @pause="playing = false" @ended="playing = false" @error="emit('error')" />
      <button v-if="!playing" class="ycloud-video-play" type="button" :aria-label="locale.text('播放视频', 'Play video')" @click="togglePlayback"><PhPlay :size="29" weight="fill" /></button>
      <div class="ycloud-video-controls">
        <div class="ycloud-video-progress">
          <span class="ycloud-video-progress-fill" :style="{ width: `${duration ? Math.min(100, currentTime / duration * 100) : 0}%` }" />
          <input class="ycloud-media-seek" type="range" min="0" :max="duration || 0" step="0.1" :value="currentTime" :disabled="!duration" :aria-label="locale.text('播放进度', 'Playback position')" @input="seek">
        </div>
        <div class="ycloud-video-actions">
          <button type="button" :aria-label="locale.text('后退 10 秒', 'Back 10 seconds')" @click="skip(-10)"><PhSkipBack :size="20" weight="fill" /></button>
          <button type="button" :aria-label="playing ? locale.text('暂停', 'Pause') : locale.text('播放', 'Play')" @click="togglePlayback"><PhPause v-if="playing" :size="20" weight="fill" /><PhPlay v-else :size="20" weight="fill" /></button>
          <button type="button" :aria-label="locale.text('前进 10 秒', 'Forward 10 seconds')" @click="skip(10)"><PhSkipForward :size="20" weight="fill" /></button>
          <button type="button" :aria-label="muted ? locale.text('取消静音', 'Unmute') : locale.text('静音', 'Mute')" @click="toggleMute"><PhSpeakerX v-if="muted" :size="21" /><PhSpeakerHigh v-else :size="21" /></button>
          <span class="ycloud-media-time">{{ formatTime(currentTime) }} / {{ formatTime(duration) }}</span>
          <span class="ycloud-media-spacer" />
          <div class="ycloud-video-volume">
            <span class="ycloud-video-volume-fill" :style="{ width: `${Math.round((muted ? 0 : volume) * 100)}%` }" />
            <input class="ycloud-media-volume" type="range" min="0" max="1" step="0.05" :value="muted ? 0 : volume" :aria-label="locale.text('音量', 'Volume')" @input="changeVolume">
          </div>
          <button type="button" :aria-label="locale.text('全屏', 'Fullscreen')" @click="toggleFullscreen"><PhArrowsOut :size="21" /></button>
        </div>
      </div>
    </div>
    <template v-else>
      <audio ref="media" :src="src" preload="metadata" @loadedmetadata="syncProgress" @durationchange="syncProgress" @timeupdate="syncProgress" @volumechange="syncProgress" @play="playing = true" @pause="playing = false" @ended="onEnded" @error="emit('error')" />
      <div class="ycloud-audio-transport">
        <button v-if="hasPrevious !== undefined" type="button" :disabled="!hasPrevious" :aria-label="locale.text('上一首', 'Previous track')" @click="emit('previous')"><AppIcon name="skip-back-circle" :size="20" weight="fill" /></button>
        <button class="ycloud-audio-play" type="button" :aria-label="playing ? locale.text('暂停', 'Pause') : locale.text('播放', 'Play')" @click="togglePlayback"><AppIcon :name="playing ? 'pause-circle' : 'play-circle'" :size="24" weight="fill" /></button>
        <button v-if="hasNext !== undefined" type="button" :disabled="!hasNext" :aria-label="locale.text('下一首', 'Next track')" @click="emit('next')"><AppIcon name="skip-forward-circle" :size="20" weight="fill" /></button>
      </div>
      <div class="ycloud-audio-track">
        <strong :title="name">{{ name.replace(/\.[^.]+$/, '') }}</strong>
        <div class="ycloud-audio-progress">
          <span class="ycloud-audio-progress-fill" :style="{ width: `${duration ? currentTime / duration * 100 : 0}%` }" />
          <input class="ycloud-media-seek" type="range" min="0" :max="duration || 0" step="0.1" :value="currentTime" :disabled="!duration" :aria-label="locale.text('播放进度', 'Playback position')" @input="seek">
        </div>
      </div>
      <span class="visually-hidden">{{ formatTime(currentTime) }} / {{ formatTime(duration) }}</span>
      <button class="ycloud-audio-loop" :class="{ active: looping }" type="button" :aria-label="looping ? locale.text('关闭循环播放', 'Turn off repeat') : locale.text('循环播放', 'Repeat track')" :title="looping ? locale.text('关闭循环播放', 'Turn off repeat') : locale.text('循环播放', 'Repeat track')" :aria-pressed="looping" @click="toggleLoop"><AppIcon name="repeat" :size="19" weight="fill" /></button>
      <div ref="volumeControl" class="ycloud-audio-volume" :class="{ 'is-open': volumeOpen }" @keydown.esc.stop.prevent="closeVolume">
        <button ref="volumeButton" type="button" :aria-label="locale.text('调整音量', 'Adjust volume')" :title="locale.text('调整音量', 'Adjust volume')" :aria-expanded="volumeOpen" aria-controls="ycloud-audio-volume-panel" @click="volumeOpen = !volumeOpen"><AppIcon :name="muted ? 'volume-off' : 'volume'" :size="19" weight="fill" /></button>
        <Transition name="ycloud-volume">
          <div v-if="volumeOpen" id="ycloud-audio-volume-panel" class="ycloud-audio-volume-popover">
            <span>{{ Math.round((muted ? 0 : volume) * 100) }}%</span>
            <div class="ycloud-audio-volume-rail">
              <input class="ycloud-media-volume" type="range" min="0" max="1" step="0.01" :value="muted ? 0 : volume" :style="{ '--volume-fill': `${Math.round((muted ? 0 : volume) * 100)}%` }" :aria-label="locale.text('音量', 'Volume')" aria-orientation="vertical" @input="changeVolume">
            </div>
          </div>
        </Transition>
      </div>
      <button v-if="showClose" class="ycloud-audio-close" type="button" :aria-label="locale.text('关闭播放器', 'Close player')" @click="emit('close')"><AppIcon name="close" :size="17" weight="regular" /></button>
    </template>
  </section>
</template>

<style scoped>
.ycloud-media-player { width: min(100%, 1120px); overflow: hidden; color: var(--text); background: var(--panel); border: 1px solid var(--line); border-radius: 5px; }
.ycloud-media-player.video:fullscreen { width: 100%; height: 100%; border: 0; border-radius: 0; }
.ycloud-video-stage { position: relative; width: 100%; aspect-ratio: 16 / 9; max-height: calc(100vh - 140px); background: #080a0d; }
.ycloud-media-player.video:fullscreen .ycloud-video-stage { height: 100%; max-height: none; aspect-ratio: auto; }
.ycloud-video-stage video { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: contain; cursor: pointer; }
.ycloud-video-play { position: absolute; top: 50%; left: 50%; display: grid; place-items: center; width: 62px; height: 62px; padding: 0; transform: translate(-50%, -50%); color: var(--accent); background: white; border: 0; border-radius: 50%; cursor: pointer; }
.ycloud-video-play:hover { background: #eaf1ff; }
.ycloud-video-controls { position: absolute; inset: auto 0 0; display: grid; gap: 1px; padding: 3px 12px 5px; color: white; background: rgb(12 16 22 / 92%); border-top: 1px solid rgb(255 255 255 / 10%); }
.ycloud-video-actions { display: flex; align-items: center; gap: 5px; }
.ycloud-video-actions svg { display: block; }
.ycloud-video-progress, .ycloud-video-volume { position: relative; display: flex; align-items: center; height: 14px; }
.ycloud-video-progress { width: 100%; }
.ycloud-video-volume { flex: 0 0 76px; width: 76px; }
.ycloud-video-progress::before, .ycloud-video-volume::before { position: absolute; top: 50%; right: 0; left: 0; height: 3px; content: ''; transform: translateY(-50%); background: rgb(255 255 255 / 32%); border-radius: 999px; }
.ycloud-video-progress-fill, .ycloud-video-volume-fill { position: absolute; top: 50%; left: 0; height: 3px; transform: translateY(-50%); background: var(--accent); border-radius: 999px; pointer-events: none; }
.ycloud-media-player.video .ycloud-media-seek, .ycloud-media-player.video .ycloud-media-volume { position: relative; width: 100%; height: 14px; margin: 0; appearance: none; background: transparent; }
.ycloud-media-player.video .ycloud-media-seek::-webkit-slider-runnable-track, .ycloud-media-player.video .ycloud-media-volume::-webkit-slider-runnable-track { height: 14px; background: transparent; }
.ycloud-media-player.video .ycloud-media-seek::-moz-range-track, .ycloud-media-player.video .ycloud-media-volume::-moz-range-track { height: 14px; background: transparent; }
.ycloud-media-player.video .ycloud-media-seek::-webkit-slider-thumb, .ycloud-media-player.video .ycloud-media-volume::-webkit-slider-thumb { width: 9px; height: 9px; margin-top: 2.5px; appearance: none; background: var(--accent); border: 0; border-radius: 50%; }
.ycloud-media-player.video .ycloud-media-seek::-moz-range-thumb, .ycloud-media-player.video .ycloud-media-volume::-moz-range-thumb { width: 9px; height: 9px; background: var(--accent); border: 0; border-radius: 50%; }
.ycloud-media-player.video .ycloud-media-seek:disabled { cursor: default; }
.ycloud-media-player button:not(.ycloud-video-play) { display: grid; place-items: center; flex: 0 0 auto; width: 34px; height: 34px; padding: 0; color: inherit; background: transparent; border: 0; border-radius: 3px; cursor: pointer; }
.ycloud-media-player button:not(.ycloud-video-play):hover { background: var(--panel-soft); }
.ycloud-media-player.video button:not(.ycloud-video-play) { width: 32px; height: 32px; color: white; border-radius: 5px; }
.ycloud-media-player.video button:not(.ycloud-video-play):hover { background: rgb(255 255 255 / 12%); }
.ycloud-media-player.video .ycloud-media-time { color: rgb(255 255 255 / 78%); }
.ycloud-media-player button:disabled { opacity: .38; cursor: not-allowed; }
.ycloud-media-player button:disabled:hover { background: transparent; }
.ycloud-media-player input { accent-color: var(--accent); cursor: pointer; }
.ycloud-media-seek { width: 100%; min-width: 40px; height: 16px; margin: 0; }
.ycloud-media-volume { width: 78px; }
.ycloud-media-time { flex: 0 0 auto; color: var(--muted); font-size: 12px; font-variant-numeric: tabular-nums; white-space: nowrap; }
.ycloud-media-spacer { flex: 1; }
.ycloud-media-player.audio { display: flex; align-items: center; gap: 4px; width: min(100%, 380px); height: 42px; min-height: 0; overflow: visible; padding: 4px 8px; color: var(--text); background: var(--panel-soft); border: 1px solid var(--line); border-radius: 9px; }
.ycloud-media-player.audio button:not(.ycloud-video-play) { width: 23px; height: 27px; color: var(--audio-ink); }
.ycloud-media-player.audio :deep(.app-icon) { color: currentColor !important; }
.ycloud-media-player.audio button:not(.ycloud-video-play):hover { color: var(--audio-accent); background: var(--audio-accent-soft); }
.ycloud-media-player.audio button:disabled { opacity: .38; }
.ycloud-audio-transport { display: flex; align-items: center; gap: 0; flex: 0 0 auto; margin-right: 1px; }
.ycloud-media-player .ycloud-audio-play { width: 31px !important; height: 31px !important; }
.ycloud-media-player.audio .ycloud-audio-play { color: var(--audio-accent) !important; }
.ycloud-audio-track { display: grid; grid-template-rows: 14px 3px; align-content: center; gap: 5px; min-width: 0; flex: 1; }
.ycloud-audio-track strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11px; font-weight: 650; }
.ycloud-audio-progress { position: relative; height: 2px; align-self: end; background: var(--line-strong); border-radius: 2px; }
.ycloud-audio-progress-fill { position: absolute; inset: 0 auto 0 0; background: var(--accent); border-radius: inherit; }
.ycloud-audio-progress .ycloud-media-seek { position: absolute; inset: -5px 0; width: 100%; height: 13px; margin: 0; appearance: none; background: transparent; }
.ycloud-audio-progress .ycloud-media-seek::-webkit-slider-runnable-track { height: 13px; background: transparent; }
.ycloud-audio-progress .ycloud-media-seek::-moz-range-track { height: 13px; background: transparent; }
.ycloud-audio-progress .ycloud-media-seek::-webkit-slider-thumb { width: 8px; height: 8px; margin-top: 2px; appearance: none; opacity: 0; background: var(--accent); border: 0; border-radius: 50%; }
.ycloud-audio-progress .ycloud-media-seek::-moz-range-thumb { width: 8px; height: 8px; opacity: 0; background: var(--accent); border: 0; border-radius: 50%; }
.ycloud-audio-progress:hover .ycloud-media-seek::-webkit-slider-thumb, .ycloud-audio-progress:focus-within .ycloud-media-seek::-webkit-slider-thumb { opacity: 1; }
.ycloud-audio-progress:hover .ycloud-media-seek::-moz-range-thumb, .ycloud-audio-progress:focus-within .ycloud-media-seek::-moz-range-thumb { opacity: 1; }
.ycloud-media-player.audio .ycloud-audio-loop.active { color: var(--audio-accent); background: var(--audio-accent-soft); }
.ycloud-audio-volume { position: relative; flex: 0 0 auto; }
.ycloud-media-player.audio .ycloud-audio-volume.is-open > button { position: relative; z-index: 2; color: var(--audio-accent); background: var(--panel); border-radius: 0 0 7px 7px; }
.ycloud-audio-volume-popover { position: absolute; z-index: 1; bottom: calc(100% + 3px); left: 50%; display: flex; flex-direction: column; align-items: center; gap: 5px; width: 30px; padding: 9px 3px 10px; transform: translateX(-50%); color: var(--audio-ink); background: var(--panel); border: 1px solid var(--line); border-radius: 15px; box-shadow: 0 8px 20px rgb(0 0 0 / 12%); }
.ycloud-audio-volume-popover span { font-size: 10px; font-variant-numeric: tabular-nums; line-height: 13px; white-space: nowrap; }
.ycloud-audio-volume-rail { position: relative; width: 16px; height: 82px; }
.ycloud-audio-volume .ycloud-media-volume { position: absolute; top: 50%; left: 50%; width: 82px; height: 16px; margin: 0; transform: translate(-50%, -50%) rotate(-90deg); appearance: none; background: linear-gradient(to right, var(--audio-accent) var(--volume-fill), var(--line-strong) var(--volume-fill)); background-position: center; background-repeat: no-repeat; background-size: 100% 1.5px; }
.ycloud-audio-volume .ycloud-media-volume::-webkit-slider-runnable-track { height: 16px; background: transparent; }
.ycloud-audio-volume .ycloud-media-volume::-webkit-slider-thumb { width: 8px; height: 8px; margin-top: 4px; appearance: none; background: var(--audio-accent); border: 0; border-radius: 50%; }
.ycloud-audio-volume .ycloud-media-volume::-moz-range-track { height: 16px; background: transparent; }
.ycloud-audio-volume .ycloud-media-volume::-moz-range-thumb { width: 8px; height: 8px; background: var(--audio-accent); border: 0; border-radius: 50%; }
.ycloud-volume-enter-active, .ycloud-volume-leave-active { transition: opacity .16s ease, transform .16s ease; transform-origin: center bottom; }
.ycloud-volume-enter-from, .ycloud-volume-leave-to { opacity: 0; transform: translateX(-50%) translateY(8px) scale(.92); }
.ycloud-media-player.audio .ycloud-audio-close { color: var(--muted); }
@media (max-width: 700px) { .ycloud-video-controls { padding: 3px 8px 5px; } .ycloud-video-actions { gap: 2px; } }
@media (max-width: 520px) { .ycloud-video-volume { display: none; } .ycloud-media-player.video button:not(.ycloud-video-play) { width: 29px; height: 29px; } }
@media (max-width: 520px) { .ycloud-media-player.audio { gap: 3px; padding: 4px 6px; } }
@media (prefers-reduced-motion: reduce) { .ycloud-volume-enter-active, .ycloud-volume-leave-active { transition: none; } }
</style>
