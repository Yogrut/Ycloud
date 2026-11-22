/**
 * True when the pointer sits in the scrollbar gutter rather than over the
 * content. Overlay scrollbars take no width, so the check simply never fires
 * there and selection keeps its normal behaviour.
 */
export function isScrollbarHit(container: HTMLElement, clientX: number): boolean {
  // Only a bar that takes layout width has a gutter; overlay bars and layouts
  // that cannot be measured (jsdom) report equal widths.
  if (container.offsetWidth <= container.clientWidth) return false
  const rect = container.getBoundingClientRect()
  return clientX > rect.left + container.clientWidth
}
