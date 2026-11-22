import { describe, expect, it } from 'vitest'
import { isScrollbarHit } from './scrollbarHit'

function container(clientWidth: number, offsetWidth: number, right: number): HTMLElement {
  const element = document.createElement('div')
  Object.defineProperty(element, 'clientWidth', { configurable: true, value: clientWidth })
  Object.defineProperty(element, 'offsetWidth', { configurable: true, value: offsetWidth })
  element.getBoundingClientRect = () => ({ left: 0, right } as DOMRect)
  return element
}

describe('isScrollbarHit', () => {
  it('treats the gutter next to the content as a scrollbar press', () => {
    expect(isScrollbarHit(container(400, 408, 408), 404)).toBe(true)
  })

  it('keeps presses inside the content selectable', () => {
    expect(isScrollbarHit(container(400, 408, 408), 399)).toBe(false)
  })

  it('never treats an overlay scrollbar as a gutter', () => {
    expect(isScrollbarHit(container(408, 408, 408), 404)).toBe(false)
  })

  it('ignores an unmeasured container instead of blocking every press', () => {
    expect(isScrollbarHit(container(0, 0, 0), 120)).toBe(false)
  })
})
