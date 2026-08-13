'use strict';

const THEME_KEY = 'ycloud-theme';
const root = document.documentElement;
const SVG_NS = 'http://www.w3.org/2000/svg';

function createThemeIcon(theme) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  const shapes = theme === 'dark'
    ? [['path', { d: 'M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401' }]]
    : [
        ['circle', { cx: '12', cy: '12', r: '4' }],
        ['path', { d: 'M12 2v2' }], ['path', { d: 'M12 20v2' }],
        ['path', { d: 'm4.93 4.93 1.41 1.41' }], ['path', { d: 'm17.66 17.66 1.41 1.41' }],
        ['path', { d: 'M2 12h2' }], ['path', { d: 'M20 12h2' }],
        ['path', { d: 'm6.34 17.66-1.41 1.41' }], ['path', { d: 'm19.07 4.93-1.41 1.41' }]
      ];
  for (const [tag, attributes] of shapes) {
    const shape = document.createElementNS(SVG_NS, tag);
    for (const [name, value] of Object.entries(attributes)) shape.setAttribute(name, value);
    svg.append(shape);
  }
  return svg;
}

function preferredTheme() {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === 'light' || saved === 'dark') return saved;
  } catch (_) { /* fall back to the product default */ }
  return 'light';
}

function updateThemeButtons(theme) {
  const nextTheme = theme === 'dark' ? '白天模式' : '黑夜模式';
  for (const button of document.querySelectorAll('[data-theme-toggle]')) {
    button.title = `切换到${nextTheme}`;
    button.setAttribute('aria-label', `切换到${nextTheme}`);
    button.querySelector('.theme-glyph')?.replaceChildren(createThemeIcon(theme));
  }
}

function applyTheme(theme, persist = false) {
  root.dataset.theme = theme;
  if (persist) {
    try { localStorage.setItem(THEME_KEY, theme); } catch (_) { /* theme remains active */ }
  }
  updateThemeButtons(theme);
}

applyTheme(preferredTheme());

document.addEventListener('DOMContentLoaded', () => {
  updateThemeButtons(root.dataset.theme);
  for (const button of document.querySelectorAll('[data-theme-toggle]')) {
    button.addEventListener('click', () => {
      applyTheme(root.dataset.theme === 'dark' ? 'light' : 'dark', true);
    });
  }
});
