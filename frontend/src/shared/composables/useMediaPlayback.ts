import { computed, readonly, ref, watch, type Ref } from 'vue'

interface PlaybackTarget {
  element: HTMLMediaElement
  pending: boolean
}

// Owns one DOM media element at a time; the component owns layout and navigation.
export function useMediaPlayback(
  media: Ref<HTMLMediaElement | null>,
  autoplay: Readonly<Ref<boolean | undefined>>,
  onError: () => void,
  onEnded: () => void,
) {
  const playing = ref(false)
  const looping = ref(false)
  const muted = ref(false)
  const volume = ref(1)
  const currentTime = ref(0)
  const duration = ref(0)
  const progressPercent = computed(() => duration.value ? Math.min(100, currentTime.value / duration.value * 100) : 0)
  let current: PlaybackTarget | undefined

  function syncProgress(): void {
    const element = current?.element
    if (!element) return
    currentTime.value = Number.isFinite(element.currentTime) ? Math.max(0, element.currentTime) : 0
    duration.value = Number.isFinite(element.duration) ? Math.max(0, element.duration) : 0
    volume.value = element.volume
    muted.value = element.muted
    looping.value = element.loop
  }

  async function play(target: PlaybackTarget, reportFailure: boolean): Promise<void> {
    if (target.pending) return
    target.pending = true
    try {
      await target.element.play()
    } catch (error) {
      // pause()/load() can interrupt play without indicating a corrupt file.
      if (current === target && reportFailure && !(error instanceof DOMException && error.name === 'AbortError')) onError()
    } finally {
      target.pending = false
    }
  }

  watch(media, (element, _previous, onCleanup) => {
    playing.value = false
    looping.value = false
    muted.value = false
    volume.value = 1
    currentTime.value = 0
    duration.value = 0
    if (!element) return
    const target: PlaybackTarget = { element, pending: false }
    current = target
    const events = {
      loadedmetadata: syncProgress,
      durationchange: syncProgress,
      timeupdate: syncProgress,
      volumechange: syncProgress,
      play: () => { playing.value = true },
      pause: () => { playing.value = false },
      ended: () => { playing.value = false; onEnded() },
      error: onError,
    }
    for (const [name, listener] of Object.entries(events)) element.addEventListener(name, listener)
    onCleanup(() => {
      if (current === target) current = undefined
      for (const [name, listener] of Object.entries(events)) element.removeEventListener(name, listener)
      element.pause()
      // Reset loading as well as playback, releasing the obsolete source.
      element.removeAttribute('src')
      element.load()
    })
    syncProgress()
    if (autoplay.value) void play(target, false)
  }, { flush: 'sync' })

  async function togglePlayback(): Promise<void> {
    const target = current
    if (!target) return
    if (!target.element.paused) target.element.pause()
    else await play(target, true)
  }

  function seekTo(seconds: number): void {
    if (!current || !duration.value || !Number.isFinite(seconds)) return
    current.element.currentTime = Math.max(0, Math.min(duration.value, seconds))
    syncProgress()
  }

  function skip(seconds: number): void {
    if (current) seekTo(current.element.currentTime + seconds)
  }

  function seek(event: Event): void {
    seekTo(Number((event.target as HTMLInputElement).value))
  }

  function changeVolume(event: Event): void {
    const nextVolume = Number((event.target as HTMLInputElement).value)
    if (!current || !Number.isFinite(nextVolume)) return
    current.element.volume = Math.max(0, Math.min(1, nextVolume))
    current.element.muted = current.element.volume === 0
    syncProgress()
  }

  function toggleMute(): void {
    if (!current) return
    current.element.muted = !current.element.muted
    syncProgress()
  }

  function toggleLoop(): void {
    if (!current) return
    current.element.loop = !current.element.loop
    syncProgress()
  }

  return {
    playing: readonly(playing), looping: readonly(looping), muted: readonly(muted),
    volume: readonly(volume), currentTime: readonly(currentTime), duration: readonly(duration),
    progressPercent, togglePlayback, skip, seek, changeVolume, toggleMute, toggleLoop,
  }
}
