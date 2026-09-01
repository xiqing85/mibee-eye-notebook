// Day/night theme with system-preference default, persisted locally.

import { store, setTheme } from './store.js';
import { t } from './i18n.js';

export function applyTheme() {
  if (!localStorage.getItem('mibee_theme')) {
    store.theme = window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  document.documentElement.dataset.theme = store.theme;
  const dark = store.theme === 'dark';
  document.querySelectorAll('.theme-btn').forEach((b) => {
    b.setAttribute('aria-pressed', String(dark));
    b.setAttribute('aria-label', t('themeLabel'));
    b.querySelectorAll('.icon-sun, .icon-moon').forEach((n) => n.classList.add('hidden'));
    const show = dark ? b.querySelector('.icon-moon') : b.querySelector('.icon-sun');
    if (show) show.classList.remove('hidden');
  });
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute('content', dark ? '#070b12' : '#eef1f5');
}

export function initTheme() {
  applyTheme();
  document.querySelectorAll('.theme-btn').forEach((b) => {
    b.addEventListener('click', () => {
      setTheme(store.theme === 'dark' ? 'light' : 'dark');
      applyTheme();
    });
  });
}
