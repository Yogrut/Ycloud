import { createApp, h, nextTick, reactive } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import MediaPlayer from './MediaPlayer.vue'

const apps: Array<ReturnType<typeof createApp>> = []

function createTestApp(render: () => ReturnType<typeof h> | Array<ReturnType<typeof h>>) {
  const app = createApp({ render })
  apps.push(app)
  return app
}

function mountPlayer(kind: 'audio' | 'video' = 'audio', autoplay = false) {
  const props = reactive({ src: '/first.mp3', name: 'first.mp3', kind, autoplay, hasNext: true })
  const error = vi.fn()
  const next = vi.fn()
  const host = document.createElement('div')
  document.body.append(host)
  const app = createTestApp(() => h(MediaPlayer, { ...props, onError: error, onNext: next }))
  app.mount(host)
  return { props, host, error, next, app, element: () => host.querySelector<HTMLMediaElement>('audio, video')! }
}

function progress(element: HTMLMediaElement, seconds: number, duration: number) {
  Object.defineProperty(element, 'duration', { configurable: true, value: duration })
  element.currentTime = seconds
  element.dispatchEvent(new Event('timeupdate'))
}

function click(host: HTMLElement, selector: string) {
  host.querySelector<HTMLButtonElement>(selector)!.click()
}

async function settle() {
  await Promise.resolve()
  await nextTick()
}

beforeEach(() => {
  vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined)
  vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => undefined)
  vi.spyOn(HTMLMediaElement.prototype, 'load').mockImplementation(() => undefined)
})

afterEach(() => {
  apps.splice(0).forEach(app => app.unmount())
  vi.restoreAllMocks()
  document.body.replaceChildren()
})

describe('MediaPlayer', () => {
  it.each(['audio', 'video'] as const)('bounds the %s progress display', async kind => {
    const player = mountPlayer(kind)
    progress(player.element(), 110, 100)
    await nextTick()
    expect(player.host.querySelector<HTMLElement>(`.ycloud-${kind}-progress-fill`)!.style.width).toBe('100%')
    progress(player.element(), -10, 100)
    await nextTick()
    expect(player.host.querySelector<HTMLElement>(`.ycloud-${kind}-progress-fill`)!.style.width).toBe('0%')
  })

  it('does not enable seeking for a negative or unknown duration', async () => {
    const player = mountPlayer()
    for (const duration of [-1, NaN, Infinity]) {
      progress(player.element(), 5, duration)
      await nextTick()
      expect(player.host.querySelector<HTMLInputElement>('.ycloud-media-seek')!.disabled).toBe(true)
    }
  })

  it('seeks and skips within the available duration', async () => {
    const player = mountPlayer('video')
    progress(player.element(), 95, 100)
    await nextTick()
    click(player.host, 'button[aria-label="前进 10 秒"]')
    expect(player.element().currentTime).toBe(100)
    click(player.host, 'button[aria-label="后退 10 秒"]')
    expect(player.element().currentTime).toBe(90)
    const input = player.host.querySelector<HTMLInputElement>('.ycloud-media-seek')!
    input.value = '25'
    input.dispatchEvent(new Event('input'))
    expect(player.element().currentTime).toBe(25)
  })

  it('shares volume, mute and repeat state with the media element', async () => {
    const player = mountPlayer('video')
    const volume = player.host.querySelector<HTMLInputElement>('.ycloud-media-volume')!
    volume.value = '0'
    volume.dispatchEvent(new Event('input'))
    expect(player.element().muted).toBe(true)
    await nextTick()
    click(player.host, 'button[aria-label="取消静音"]')
    expect(player.element().muted).toBe(false)
    player.props.kind = 'audio'
    await nextTick()
    click(player.host, '.ycloud-audio-loop')
    await nextTick()
    expect(player.element().loop).toBe(true)
    expect(player.host.querySelector('.ycloud-audio-loop')!.getAttribute('aria-pressed')).toBe('true')
  })

  it('does not start duplicate play requests while one is pending', async () => {
    let resolve!: () => void
    const play = vi.mocked(HTMLMediaElement.prototype.play).mockReturnValue(new Promise<void>(done => { resolve = done }))
    const player = mountPlayer()
    click(player.host, '.ycloud-audio-play')
    click(player.host, '.ycloud-audio-play')
    expect(play).toHaveBeenCalledTimes(1)
    resolve()
    await settle()
    click(player.host, '.ycloud-audio-play')
    expect(play).toHaveBeenCalledTimes(2)
  })

  it('reflects play/pause events and pauses an active element', async () => {
    const player = mountPlayer()
    Object.defineProperty(player.element(), 'paused', { configurable: true, value: false })
    player.element().dispatchEvent(new Event('play'))
    await nextTick()
    expect(player.host.querySelector('.ycloud-audio-play')!.getAttribute('aria-label')).toBe('暂停')
    click(player.host, '.ycloud-audio-play')
    expect(HTMLMediaElement.prototype.pause).toHaveBeenCalledTimes(1)
    player.element().dispatchEvent(new Event('pause'))
    await nextTick()
    expect(player.host.querySelector('.ycloud-audio-play')!.getAttribute('aria-label')).toBe('播放')
  })

  it('reports a current playback failure', async () => {
    vi.mocked(HTMLMediaElement.prototype.play).mockRejectedValue(new Error('decode failed'))
    const player = mountPlayer()
    click(player.host, '.ycloud-audio-play')
    await settle()
    expect(player.error).toHaveBeenCalledTimes(1)
  })

  it('does not report an interrupted play request as a broken file', async () => {
    vi.mocked(HTMLMediaElement.prototype.play).mockRejectedValue(new DOMException('interrupted', 'AbortError'))
    const player = mountPlayer()
    click(player.host, '.ycloud-audio-play')
    await settle()
    expect(player.error).not.toHaveBeenCalled()
  })

  it.each(['unmount', 'replace'] as const)('ignores a late play failure after %s', async action => {
    let reject!: (reason: Error) => void
    vi.mocked(HTMLMediaElement.prototype.play).mockReturnValue(new Promise<void>((_done, fail) => { reject = fail }))
    const player = mountPlayer()
    click(player.host, '.ycloud-audio-play')
    if (action === 'unmount') {
      apps.splice(apps.indexOf(player.app), 1)
      player.app.unmount()
    } else {
      player.props.src = '/second.mp3'
      await nextTick()
    }
    reject(new Error('old failure'))
    await settle()
    expect(player.error).not.toHaveBeenCalled()
  })

  it('releases the old media source and ignores its events on replacement', async () => {
    const player = mountPlayer()
    const old = player.element()
    progress(old, 40, 100)
    old.loop = true
    old.dispatchEvent(new Event('play'))
    player.props.src = '/second.mp3'
    await nextTick()
    expect(player.element()).not.toBe(old)
    expect(old.hasAttribute('src')).toBe(false)
    expect(HTMLMediaElement.prototype.pause).toHaveBeenCalledTimes(1)
    expect(HTMLMediaElement.prototype.load).toHaveBeenCalledTimes(1)
    old.dispatchEvent(new Event('error'))
    old.dispatchEvent(new Event('ended'))
    expect(player.error).not.toHaveBeenCalled()
    expect(player.next).not.toHaveBeenCalled()
    expect(player.host.querySelector<HTMLInputElement>('.ycloud-media-seek')!.disabled).toBe(true)
    expect(player.host.querySelector('.ycloud-audio-loop')!.getAttribute('aria-pressed')).toBe('false')
  })

  it('releases media loading when the player is unmounted', () => {
    const player = mountPlayer()
    const element = player.element()
    apps.splice(apps.indexOf(player.app), 1)
    player.app.unmount()
    expect(element.hasAttribute('src')).toBe(false)
    expect(HTMLMediaElement.prototype.pause).toHaveBeenCalledTimes(1)
    expect(HTMLMediaElement.prototype.load).toHaveBeenCalledTimes(1)
    element.dispatchEvent(new Event('error'))
    expect(player.error).not.toHaveBeenCalled()
  })

  it('tries autoplay once for each source and silently tolerates denial', async () => {
    const play = vi.mocked(HTMLMediaElement.prototype.play).mockRejectedValue(new DOMException('gesture required', 'NotAllowedError'))
    const player = mountPlayer('audio', true)
    await settle()
    expect(play).toHaveBeenCalledTimes(1)
    player.props.name = 'new label.mp3'
    await nextTick()
    expect(play).toHaveBeenCalledTimes(1)
    player.props.src = '/second.mp3'
    await settle()
    expect(play).toHaveBeenCalledTimes(2)
    expect(player.error).not.toHaveBeenCalled()
  })

  it('closes the volume popup outside after switching from video to audio', async () => {
    const player = mountPlayer('video')
    player.props.kind = 'audio'
    await nextTick()
    click(player.host, 'button[aria-label="调整音量"]')
    await nextTick()
    document.body.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }))
    await nextTick()
    expect(player.host.querySelector('button[aria-label="调整音量"]')!.getAttribute('aria-expanded')).toBe('false')
  })

  it('assigns a distinct volume popup ID to each player', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = createTestApp(() => [
      h(MediaPlayer, { src: '/one.mp3', name: 'one.mp3', kind: 'audio' }),
      h(MediaPlayer, { src: '/two.mp3', name: 'two.mp3', kind: 'audio' }),
    ])
    app.mount(host)
    const buttons = host.querySelectorAll<HTMLButtonElement>('button[aria-label="调整音量"]')
    buttons.forEach(button => button.click())
    await nextTick()
    const panels = host.querySelectorAll('.ycloud-audio-volume-popover')
    const firstId = panels[0]!.id
    const secondId = panels[1]!.id
    expect(firstId).not.toBe(secondId)
    expect(buttons[0]!.getAttribute('aria-controls')).toBe(firstId)
  })

  it('moves to the next audio track only when available', async () => {
    const player = mountPlayer()
    player.element().dispatchEvent(new Event('ended'))
    expect(player.next).toHaveBeenCalledTimes(1)
    player.props.hasNext = false
    await nextTick()
    player.element().dispatchEvent(new Event('ended'))
    expect(player.next).toHaveBeenCalledTimes(1)
    player.props.kind = 'video'
    player.props.hasNext = true
    await nextTick()
    player.element().dispatchEvent(new Event('ended'))
    expect(player.next).toHaveBeenCalledTimes(1)
  })

  it('does not accept an old play failure after returning to the same URL', async () => {
    let reject!: (reason: Error) => void
    const play = vi.mocked(HTMLMediaElement.prototype.play)
      .mockReturnValueOnce(new Promise<void>((_done, fail) => { reject = fail }))
      .mockResolvedValue(undefined)
    const player = mountPlayer()
    click(player.host, '.ycloud-audio-play')
    player.props.src = '/second.mp3'
    await nextTick()
    player.props.src = '/first.mp3'
    await nextTick()
    click(player.host, '.ycloud-audio-play')
    reject(new Error('first attempt failed'))
    await settle()
    expect(play).toHaveBeenCalledTimes(2)
    expect(player.error).not.toHaveBeenCalled()
  })

  it('keeps volume controls usable inside and restores focus on Escape', async () => {
    const player = mountPlayer()
    const button = player.host.querySelector<HTMLButtonElement>('button[aria-label="调整音量"]')!
    button.click()
    await nextTick()
    const input = player.host.querySelector<HTMLInputElement>('.ycloud-media-volume')!
    input.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }))
    input.value = '0.4'
    input.dispatchEvent(new Event('input'))
    await nextTick()
    expect(button.getAttribute('aria-expanded')).toBe('true')
    expect(player.element().volume).toBe(0.4)
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await nextTick()
    expect(button.getAttribute('aria-expanded')).toBe('false')
    expect(document.activeElement).toBe(button)
  })

  it('closes the volume popup and releases audio when changing media kind', async () => {
    const player = mountPlayer()
    const old = player.element()
    click(player.host, 'button[aria-label="调整音量"]')
    player.props.kind = 'video'
    await nextTick()
    expect(old.hasAttribute('src')).toBe(false)
    old.dispatchEvent(new Event('error'))
    expect(player.error).not.toHaveBeenCalled()
    player.props.kind = 'audio'
    await nextTick()
    expect(player.host.querySelector('button[aria-label="调整音量"]')!.getAttribute('aria-expanded')).toBe('false')
  })
})
