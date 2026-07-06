// Top navigation bar: brand, route links, theme + language toggles, logout.
import { h } from 'preact';
import { t } from '../app.jsx';
import { navigate } from './useHashRoute.js';
import { theme, lang } from '../store.js';
import { api } from '../api.js';
import { showToast } from '../store.js';

export function Navbar({ onLogout }) {
  const cur = window.location.hash;
  const isActive = (r) => cur === r || (r === '#db' && (cur === '' || cur === '#'));

  async function toggleTheme() {
    theme.value = theme.value === 'dark' ? 'light' : 'dark';
    applyTheme();
    try { await api.put('/api/settings', { settings: { 'ui.theme': theme.value } }); } catch {}
  }
  async function toggleLang() {
    lang.value = lang.value === 'en' ? 'zh' : 'en';
    applyTheme();
    try { await api.put('/api/settings', { settings: { 'ui.language': lang.value } }); } catch {}
    // Force re-render of the whole tree by toggling a no-op hash twice.
    navigate(window.location.hash || '#db');
  }
  function applyTheme() {
    document.documentElement.setAttribute('data-theme', theme.value);
    document.documentElement.lang = lang.value === 'zh' ? 'zh-CN' : 'en';
  }
  // Apply theme/lang on every render so toggles reflect immediately.
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
          {theme.value === 'light' ? '☀' : '☾'}
        </button>
        <button onClick={toggleLang} aria-label={t('lang.toggle')} title={t('lang.toggle')}
                style="font-family:var(--mo);font-weight:600">
          {lang.value === 'en' ? '中' : 'EN'}
        </button>
        <button onClick={onLogout} aria-label={t('nav.logout')}>{t('nav.logout')}</button>
      </div>
    </nav>
  );
}
