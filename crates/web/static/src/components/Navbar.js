// Top navigation bar: brand, route links, theme + language toggles, logout.
import { h } from 'preact';
import { t } from '../app.jsx';
import { navigate } from './useHashRoute.js';
import { theme, lang, showToast } from '../store.js';
import { api } from '../api.js';
import { useStore } from '../hooks.js';

export function Navbar({ onLogout }) {
  // Subscribe so theme/lang toggles re-render the bar immediately.
  const currentTheme = useStore(theme);
  const currentLang = useStore(lang);
  const cur = window.location.hash;
  const isActive = (r) => cur === r || (r === '#db' && (cur === '' || cur === '#'));

  async function toggleTheme() {
    theme.value = currentTheme === 'dark' ? 'light' : 'dark';
    applyTheme();
    try { await api.put('/api/settings', { settings: { 'ui.theme': theme.value } }); } catch {}
  }
  async function toggleLang() {
    lang.value = currentLang === 'en' ? 'zh' : 'en';
    applyTheme();
    try { await api.put('/api/settings', { settings: { 'ui.language': lang.value } }); } catch {}
    // Force the whole tree to re-render so `t` picks up the new language.
    navigate(window.location.hash || '#db');
  }
  function applyTheme() {
    document.documentElement.setAttribute('data-theme', theme.value);
    document.documentElement.lang = lang.value === 'zh' ? 'zh-CN' : 'en';
  }
  applyTheme();

  return (
    <nav class="nv" aria-label="Main navigation">
      <div class="nb" onClick={() => navigate('#db')}>mibee-rec</div>
      <div class="nl">
        <a href="#db" class={isActive('#db') ? 'nl active' : 'nl'}>{t('nav.cameras')}</a>
        <a href="#st" class={isActive('#st') ? 'nl active' : 'nl'}>{t('nav.settings')}</a>
        <a href="#dv" class={isActive('#dv') ? 'nl active' : 'nl'}>{t('nav.devices')}</a>
      </div>
      <div class="nr">
        <button onClick={toggleTheme} aria-label={t('theme.toggle')} title={t('theme.toggle')}>
          {currentTheme === 'light' ? '☀' : '☾'}
        </button>
        <button onClick={toggleLang} aria-label={t('lang.toggle')} title={t('lang.toggle')}
                style="font-family:var(--mo);font-weight:600">
          {currentLang === 'en' ? '中' : 'EN'}
        </button>
        <button onClick={onLogout} aria-label={t('nav.logout')}>{t('nav.logout')}</button>
      </div>
    </nav>
  );
}
