// SPA entry point. Bootstraps auth/theme/lang, mounts the router.
//
// All views import `t` from this module so language switches propagate
// without wiring a context through every component.
import { h, render } from 'preact';
import { useEffect, useState } from 'preact/hooks';

import { makeT } from './i18n.js';
import { api, onSessionExpired } from './api.js';
import {
  authed, username, lang, theme, cameras, streamUrls, showToast,
} from './store.js';
import { useHashRoute, navigate } from './components/useHashRoute.js';
import { Navbar } from './components/Navbar.js';
import { ToastViewport } from './components/Toast.js';
import { CameraModal, ConfirmDialog } from './components/Modals.js';
import { Dashboard } from './views/Dashboard.js';
import { CameraDetail } from './views/CameraDetail.js';
import { Settings } from './views/Settings.js';
import { Devices } from './views/Devices.js';
import { Login, Setup } from './views/Auth.js';

// Active translation function — reassigned on language change.
export let t = makeT(lang.value);
function rebuildT() { t = makeT(lang.value); }

export function App() {
  const hash = useHashRoute();
  const [bootState, setBootState] = useState('checking'); // checking | login | setup | ready
  const [modal, setModal] = useState(null); // {type:'add'|'edit', camera} | {type:'delete', camera} | null
  const [, forceRender] = useState(0);

  // Boot: probe auth + first-run + theme/lang preferences.
  useEffect(() => {
    let alive = true;
    (async () => {
      // Load theme/lang preferences first so the very first paint is correct.
      const sres = await api.get('/api/settings');
      if (sres.ok && sres.data) {
        theme.value = sres.data['ui.theme'] ||
          (window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark');
        lang.value = sres.data['ui.language'] ||
          ((navigator.language || '').startsWith('zh') ? 'zh' : 'en');
      } else {
        theme.value = window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
        lang.value = (navigator.language || '').startsWith('zh') ? 'zh' : 'en';
      }
      rebuildT();
      applyTheme();
      // Force a re-render so components pick up the new `t` closure.
      forceRender((n) => n + 1);

      // Try /api/auth/me to see if we already have a session.
      const me = await api.get('/api/auth/me');
      if (!alive) return;
      if (me.ok) {
        authed.value = true;
        username.value = me.data.username;
        setBootState('ready');
      } else if (me.status === 503) {
        // Setup not yet done.
        setBootState('setup');
      } else {
        authed.value = false;
        setBootState('login');
      }
    })();
    return () => { alive = false; };
  }, []);

  // Session expiry: bump back to login.
  onSessionExpired(() => {
    authed.value = false;
    setBootState('login');
    navigate('#login');
    showToast(t('session.expired'), 'warn');
  });

  // Re-render on language change so `t` updates everywhere.
  useEffect(() => {
    const orig = lang.value;
    const id = setInterval(() => {
      if (lang.value !== orig) { rebuildT(); forceRender((n) => n + 1); }
    }, 200);
    return () => clearInterval(id);
  }, []);

  function applyTheme() {
    document.documentElement.setAttribute('data-theme', theme.value);
    document.documentElement.lang = lang.value === 'zh' ? 'zh-CN' : 'en';
  }
  useEffect(() => { applyTheme(); });

  // --- Route guard: auth/setup gating ---
  if (bootState === 'checking') {
    return <div class="boot"><div class="spinner" /></div>;
  }
  if (bootState === 'setup') {
    return (
      <div>
        <Setup onSetupDone={() => setBootState('login')} />
        <ToastViewport />
      </div>
    );
  }
  if (bootState === 'login' || hash === '#login') {
    return (
      <div>
        <Login onAuthed={async () => {
          const me = await api.get('/api/auth/me');
          if (me.ok) username.value = me.data.username;
          setBootState('ready');
          navigate('#db');
        }} />
        <ToastViewport />
      </div>
    );
  }

  // --- Authenticated routes ---
  async function handleLogout() {
    await api.post('/api/auth/logout');
    authed.value = false;
    setBootState('login');
    navigate('#login');
    showToast(t('nav.signed_out'), 'info');
  }

  // Parse the camera detail route.
  const camMatch = hash.match(/^#?\/c\/(.+)$/);
  const cameraId = camMatch ? camMatch[1] : null;

  let view;
  if (cameraId) {
    view = <CameraDetail cameraId={cameraId} />;
  } else if (hash === '#st') {
    view = <Settings />;
  } else if (hash === '#dv') {
    view = <Devices onUseAsCamera={(d) => setModal({ type: 'add', deviceIndex: d.index, deviceName: d.name })} />;
  } else {
    view = (
      <Dashboard
        onAddCamera={() => setModal({ type: 'add' })}
        onEditCamera={(c) => setModal({ type: 'edit', camera: c })}
        onDeleteCamera={(c) => setModal({ type: 'delete', camera: c })}
      />
    );
  }

  return (
    <div>
      <a href="#main" class="skip-link">{t('a11y.skip_to_content')}</a>
      <Navbar onLogout={handleLogout} />
      <main id="main">
        {view}
      </main>
      <ToastViewport />
      {modal?.type === 'add' && (
        <CameraModal
          initial={modal.deviceIndex != null ? {
            name: t('usb.name', { index: modal.deviceIndex }),
            camera_type: 'usb',
            config: { device_index: modal.deviceIndex },
          } : null}
          onClose={() => setModal(null)}
          onSaved={() => { /* Dashboard polls; nothing extra needed */ }}
        />
      )}
      {modal?.type === 'edit' && (
        <CameraModal
          initial={modal.camera}
          onClose={() => setModal(null)}
          onSaved={() => setModal(null)}
        />
      )}
      {modal?.type === 'delete' && (
        <ConfirmDialog
          title={t('camera.delete_title')}
          message={t('camera.delete_confirm', { name: modal.camera.name })}
          confirmLabel={t('common.delete')}
          onClose={() => setModal(null)}
          onConfirm={async () => {
            const res = await api.del(`/api/cameras/${modal.camera.id}`);
            if (res.ok) {
              showToast(t('camera.deleted'), 'info');
              cameras.value = cameras.value.filter((c) => c.id !== modal.camera.id);
              const next = { ...streamUrls.value };
              delete next[modal.camera.id];
              streamUrls.value = next;
            }
            setModal(null);
          }}
        />
      )}
    </div>
  );
}

// Mount.
const root = document.getElementById('app');
if (root) render(h(App), root);
