/**
 * MiBee-Rec — SPA Application Logic (reference source)
 *
 * NOTE: This file is the CANONICAL SOURCE for the SPA JavaScript.
 * The production index.html has this code inlined (and minified).
 * When making changes, update BOTH this file AND index.html.
 *
 * Naming: abbreviated function/property names match index.html.
 * Mapping table:
 *   S   = state           A   = api              T   = showToast
 *   G   = navigate        R   = getCurrentRoute  P   = showPage
 *   N   = showNavbar      U   = updateActiveNav   C   = checkAuth
 *   L   = handleLogin     H   = handleSetup      O   = handleLogout
 *   D   = loadDashboard   V   = renderCameraTable F   = formatType
 *   Y   = startStream     w   = stopStream        wa  = showAddCameraModal
 *   cm  = closeModal      sc  = saveCamera        x   = showDeleteConfirm
 *   cC  = closeConfirm    cD  = confirmDelete     K   = loadCameraView
 *   cr  = copyRtspUrl     fC  = fallbackCopy      r   = route
 *   i   = initApp         E   = html escape       J   = htmlAttr escape
 *   q   = toast queue     M   = loadSettings      Q   = saveSettings
 *
 * State properties:
 *   S.a = authenticated    S.c = cameras           S.s = settings
 *   S.dc = deleteTargetId  S.urls = stream RTSP URLs
 */

(function() {
'use strict';
let LANG = 'en';
let THEME = 'dark';
let pi = null; // polling interval handle
function t(key) { return I18N[LANG][key] || key; }
function translatePage() {
  document.querySelectorAll('[data-i18n]').forEach(function(el) {
    var key = el.getAttribute('data-i18n');
    if (key) el.textContent = t(key);
  });
  document.querySelectorAll('[data-i18n-placeholder]').forEach(function(el) {
    var key = el.getAttribute('data-i18n-placeholder');
    if (key) el.placeholder = t(key);
  });
}


function applyTheme() {
  document.documentElement.setAttribute('data-theme', THEME);
  updateThemeToggleIcon();
}
function updateThemeToggleIcon() {
  let btn = document.getElementById('theme-toggle');
  if (!btn) return;
  btn.innerHTML = THEME === 'light'
    ? '<svg width="20" height="20" viewBox="0 0 24 24"><path fill="currentColor" d="M12 7c-2.76 0-5 2.24-5 5s2.24 5 5 5 5-2.24 5-5-2.24-5-5-5zM2 13h2c.55 0 1-.45 1-1s-.45-1-1-1H2c-.55 0-1 .45-1 1s.45 1 1 1zm18 0h2c.55 0 1-.45 1-1s-.45-1-1-1h-2c-.55 0-1 .45-1 1s.45 1 1 1zM11 2v2c0 .55.45 1 1 1s1-.45 1-1V2c0-.55-.45-1-1-1s-1 .45-1 1zm0 18v2c0 .55.45 1 1 1s1-.45 1-1v-2c0-.55-.45-1-1-1s-1 .45-1 1zM5.99 4.58a.996.996 0 0 0-1.41 0 .996.996 0 0 0 0 1.41l1.06 1.06c.39.39 1.03.39 1.41 0s.39-1.03 0-1.41L5.99 4.58zm12.37 12.37a.996.996 0 0 0-1.41 0 .996.996 0 0 0 0 1.41l1.06 1.06c.39.39 1.03.39 1.41 0a.996.996 0 0 0 0-1.41l-1.06-1.06zm1.06-10.96a.996.996 0 0 0 0-1.41.996.996 0 0 0-1.41 0l-1.06 1.06c-.39.39-.39 1.03 0 1.41s1.03.39 1.41 0l1.06-1.06zM7.05 18.36a.996.996 0 0 0 0-1.41.996.996 0 0 0-1.41 0l-1.06 1.06c-.39.39-.39 1.03 0 1.41s1.03.39 1.41 0l1.06-1.06z"/></svg>'
    : '<svg width="20" height="20" viewBox="0 0 24 24"><path fill="currentColor" d="M12 3a9 9 0 1 0 9 9c0-.46-.04-.92-.1-1.36a5.389 5.389 0 0 1-4.4 2.26 5.403 5.403 0 0 1-3.14-9.8c-.44-.06-.9-.1-1.36-.1z"/></svg>';
}
function updateLangToggleText() {
  let btn = document.getElementById('lang-toggle');
  if (!btn) return;
  btn.textContent = LANG === 'en' ? '\u4e2d' : 'EN';
}
async function toggleTheme() {
  THEME = THEME === 'dark' ? 'light' : 'dark';
  applyTheme();
  try { await A('PUT', '/api/settings', { settings: { 'ui.theme': THEME } }); } catch (_) {}
}
async function toggleLang() {
  LANG = LANG === 'en' ? 'zh' : 'en';
  updateLangToggleText();
  translatePage();
  r();
  try { await A('PUT', '/api/settings', { settings: { 'ui.language': LANG } }); } catch (_) {}
}
async function initThemeAndLang() {
  try {
    let res = await A('GET', '/api/settings');
    if (res.ok && res.data) {
      THEME = res.data['ui.theme'] || (window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark');
      LANG = res.data['ui.language'] || ((navigator.language || '').startsWith('zh') ? 'zh' : 'en');
    } else {
      THEME = window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
      LANG = (navigator.language || '').startsWith('zh') ? 'zh' : 'en';
    }
  } catch (_) {
    THEME = window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
    LANG = (navigator.language || '').startsWith('zh') ? 'zh' : 'en';
  }
  applyTheme();
  updateLangToggleText();
  translatePage();
}


// ============================================================
// I18N DICTIONARY (zh-CN / en-US)
// ============================================================
const I18N = {
  'en': {
    'nav.brand': 'mibee-rec','nav.dashboard': 'Dashboard','nav.settings': 'Settings',
    'nav.devices': 'Devices','nav.logout': 'Logout','nav.signed_out': 'Signed out',
    'login.title': 'Sign In','login.subtitle': 'Camera surveillance management',
    'login.username': 'Username','login.password': 'Password','login.submit': 'Sign In',
    'login.signing_in': 'Signing in...','login.signed_in': 'Signed in',
    'login.error_empty': 'Please enter both username and password.',
    'login.error_failed': 'Login failed',
    'setup.title': 'Initial Setup','setup.subtitle': 'Create admin account',
    'setup.username': 'Admin Username','setup.password': 'Password','setup.confirm': 'Confirm Password','setup.submit': 'Create Account',
    'setup.setting_up': 'Setting up...','setup.created': 'Account created! Sign in.',
    'setup.error_empty': 'Please fill all fields.','setup.error_mismatch': 'Passwords do not match.',
    'setup.error_length': 'Password must be at least 8 characters.','setup.error_failed': 'Setup failed',
    'dashboard.title': 'Cameras','dashboard.subtitle': 'Manage surveillance cameras','dashboard.add': '+ Add Camera',
    'dashboard.loading': 'Loading cameras...','dashboard.empty_title': 'No cameras configured',
    'dashboard.empty_desc': 'Add your first camera to start monitoring.',
    'table.name': 'Name','table.type': 'Type','table.status': 'Status','table.stream_url': 'Stream URL','table.actions': 'Actions',
    'table.url_unavailable': '(URL unavailable)',
    'camera.add': 'Add Camera','camera.edit': 'Edit Camera','camera.saving': 'Saving...',
    'camera.added': 'Camera added','camera.updated': 'Camera updated','camera.deleted': 'Deleted','camera.deleting': 'Deleting...',
    'camera.delete_confirm': 'Delete "{name}"? This cannot be undone.',
    'camera.name': 'Camera Name','camera.type': 'Camera Type','camera.config': 'Config (JSON)','camera.config_hint': 'Camera-specific configuration as JSON',
    'camera.select_type': 'Select type...','camera.select_device': 'Select a device...','camera.loading_devices': 'Loading...','camera.no_devices': 'No devices found','camera.error_devices': 'Error loading devices',
    'camera.error_name': 'Name required.','camera.error_type': 'Type required.','camera.error_json': 'Invalid JSON.',
    'camera.device_label': 'USB Device','camera.use_as_camera': 'Use as Camera','camera.device_prefix': 'Device ',
    'camera.type_usb': 'USB','camera.type_rtsp': 'RTSP','camera.type_onvif': 'ONVIF','camera.type_gb28181': 'GB/T 28181','camera.type_rtmp': 'RTMP',
    'camera_view.title': 'Camera','camera_view.loading': 'Loading...','camera_view.load_failed': 'Failed to load camera',
    'camera_view.no_stream': 'No Stream Available','camera_view.start_stream': 'Start the stream to see live preview',
    'camera_view.url_copied': 'RTSP URL copied','camera_view.url_copied_short': 'URL copied',
    'camera_view.local_capture': '(local capture)','camera_view.back': '\u2190 Back',
    'caminfo.id': 'ID','caminfo.type': 'Type','caminfo.status': 'Status','caminfo.created': 'Created',
    'stream.started': 'Stream started','stream.stopped': 'Stream stopped',
    'stream.start': 'Start','stream.stop': 'Stop','stream.view': 'View','stream.edit': 'Edit','stream.delete': 'Delete','stream.copy': 'Copy',
    'settings.title': 'Settings','settings.subtitle': 'Configure system settings','settings.save': 'Save Changes','settings.saving': 'Saving...',
    'settings.saved': 'Settings saved','settings.loading': 'Loading settings...','settings.load_failed': 'Failed to load settings',
    'settings.server': 'Server','settings.web_port': 'Web UI Port','settings.rtsp_port': 'RTSP Port','settings.mibee': 'MiBee NVR',
    'settings.mibee_url': 'MiBee URL','settings.mibee_api_key': 'MiBee API Key','settings.recording': 'Recording',
    'settings.recordings_path': 'Recordings Path','settings.max_segment_duration': 'Max Segment Duration (s)',
    'settings.change_password': 'Change Password','settings.current_password': 'Current Password','settings.new_password': 'New Password','settings.confirm_new_password': 'Confirm New Password',
    'settings.change_password_btn': 'Change Password','settings.changing': 'Changing...','settings.password_changed': 'Password changed. Please sign in again.',
    'settings.password_mismatch': 'Passwords do not match.','settings.password_length': 'Password must be at least 8 characters.','settings.fields_required': 'All fields are required.',
    'protocol.onvif': 'ONVIF Configuration','protocol.gb28181': 'GB28181 Configuration','protocol.rtmp': 'RTMP Push Configuration',
    'protocol.enabled': 'Enabled','protocol.port': 'Port','protocol.device_name': 'Device Name','protocol.device_id': 'Device ID',
    'protocol.server_ip': 'Server IP','protocol.server_port': 'Server Port','protocol.url': 'Push URL',
    'protocol.save': 'Save {name}','protocol.saved': '{name} config saved','protocol.yes': 'Yes','protocol.no': 'No',
    'devices.title': 'Devices','devices.subtitle': 'Local video and audio devices','devices.video': 'Video Devices','devices.audio': 'Audio Devices',
    'devices.no_video': 'No video devices found.','devices.no_audio': 'No audio devices found.','devices.load_failed': 'Failed to load devices',
    'devices.index': 'Index: ','devices.formats': 'Formats: ','devices.configs': 'Configs: ',
    'common.cancel': 'Cancel','common.delete': 'Delete','common.copy': 'Copy','common.loading': 'Loading...','common.error': 'Error',
    'session.expired': 'Session expired',
    'usb.name': 'USB Camera {index}','usb.created': 'USB camera created','usb.create_failed': 'Failed to create camera',
    'error.network': 'Cannot reach server. Check it is running and reload.',
    'error.failed': 'Failed','error.failed_prefix': 'Failed: ','error.unknown': 'Unknown',
    'login.username_ph': 'Username','login.password_ph': 'Password',
    'setup.username_ph': 'admin','setup.password_ph': 'Strong password','setup.confirm_ph': 'Repeat password',
    'camera_view.ext_player': 'Use external RTSP player','camera_view.copy_rtsp': 'Copy RTSP URL',
    'camera.delete_title': 'Delete Camera','camera.delete_desc': 'Are you sure?',
    'camera.name_ph': 'Front Door','camera.config_ph': '{"url":"rtsp://192.168.1.100:554/stream1"}',
    'settings.web_port_ph': '9090','settings.rtsp_port_ph': '8554',
    'settings.mibee_url_ph': 'http://192.168.1.100:9090','settings.mibee_api_key_ph': 'Optional',
    'settings.recordings_path_ph': '/var/lib/mibee-rec/recordings','settings.max_segment_ph': '300',
    'settings.current_password_ph': 'Current password','settings.new_password_ph': 'New password (min 8 chars)','settings.confirm_new_password_ph': 'Repeat new password',
    'protocol.port_ph': '3702','protocol.device_name_ph': 'mibee-rec',
    'protocol.device_id_ph': '34020000001320000101','protocol.server_ip_ph': '192.168.1.100','protocol.server_port_ph': '5060','protocol.url_ph': 'rtmp://nvr-host:1935/live/stream',
    'protocol.save_onvif': 'Save ONVIF','protocol.save_gb28181': 'Save GB28181','protocol.save_rtmp': 'Save RTMP',
    'theme.toggle': 'Toggle theme','lang.toggle': 'Switch language','lang.en': 'EN','lang.zh': '\u4e2d',
  },
  'zh': {
    'nav.brand': 'mibee-rec','nav.dashboard': '\u4eea\u8868\u76d8','nav.settings': '\u8bbe\u7f6e',
    'nav.devices': '\u8bbe\u5907','nav.logout': '\u9000\u51fa\u767b\u5f55','nav.signed_out': '\u5df2\u9000\u51fa\u767b\u5f55',
    'login.title': '\u767b\u5f55','login.subtitle': '\u6444\u50cf\u5934\u76d1\u63a7\u7ba1\u7406',
    'login.username': '\u7528\u6237\u540d','login.password': '\u5bc6\u7801','login.submit': '\u767b\u5f55',
    'login.signing_in': '\u767b\u5f55\u4e2d...','login.signed_in': '\u767b\u5f55\u6210\u529f',
    'login.error_empty': '\u8bf7\u8f93\u5165\u7528\u6237\u540d\u548c\u5bc6\u7801\u3002','login.error_failed': '\u767b\u5f55\u5931\u8d25',
    'setup.title': '\u521d\u59cb\u8bbe\u7f6e','setup.subtitle': '\u521b\u5efa\u7ba1\u7406\u5458\u8d26\u6237',
    'setup.username': '\u7ba1\u7406\u5458\u7528\u6237\u540d','setup.password': '\u5bc6\u7801','setup.confirm': '\u786e\u8ba4\u5bc6\u7801','setup.submit': '\u521b\u5efa\u8d26\u6237',
    'setup.setting_up': '\u8bbe\u7f6e\u4e2d...','setup.created': '\u8d26\u6237\u5df2\u521b\u5efa\uff01\u8bf7\u767b\u5f55\u3002',
    'setup.error_empty': '\u8bf7\u586b\u5199\u6240\u6709\u5b57\u6bb5\u3002','setup.error_mismatch': '\u5bc6\u7801\u4e0d\u5339\u914d\u3002',
    'setup.error_length': '\u5bc6\u7801\u81f3\u5c11\u9700\u89818\u4e2a\u5b57\u7b26\u3002','setup.error_failed': '\u8bbe\u7f6e\u5931\u8d25',
    'dashboard.title': '\u6444\u50cf\u5934','dashboard.subtitle': '\u7ba1\u7406\u76d1\u63a7\u6444\u50cf\u5934','dashboard.add': '+ \u6dfb\u52a0\u6444\u50cf\u5934',
    'dashboard.loading': '\u52a0\u8f7d\u6444\u50cf\u5934...','dashboard.empty_title': '\u672a\u914d\u7f6e\u6444\u50cf\u5934',
    'dashboard.empty_desc': '\u6dfb\u52a0\u7b2c\u4e00\u4e2a\u6444\u50cf\u5934\u5f00\u59cb\u76d1\u63a7\u3002',
    'table.name': '\u540d\u79f0','table.type': '\u7c7b\u578b','table.status': '\u72b6\u6001','table.stream_url': '\u6d41\u5730\u5740','table.actions': '\u64cd\u4f5c',
    'table.url_unavailable': '\uff08URL\u4e0d\u53ef\u7528\uff09',
    'camera.add': '\u6dfb\u52a0\u6444\u50cf\u5934','camera.edit': '\u7f16\u8f91\u6444\u50cf\u5934','camera.saving': '\u4fdd\u5b58\u4e2d...',
    'camera.added': '\u6444\u50cf\u5934\u5df2\u6dfb\u52a0','camera.updated': '\u6444\u50cf\u5934\u5df2\u66f4\u65b0','camera.deleted': '\u5df2\u5220\u9664','camera.deleting': '\u5220\u9664\u4e2d...',
    'camera.delete_confirm': '\u5220\u9664\"{name}"\uff1f\u6b64\u64cd\u4f5c\u4e0d\u53ef\u64a4\u9500\u3002',
    'camera.name': '\u6444\u50cf\u5934\u540d\u79f0','camera.type': '\u6444\u50cf\u5934\u7c7b\u578b','camera.config': '\u914d\u7f6e\uff08JSON\uff09','camera.config_hint': '\u6444\u50cf\u5934\u7279\u5b9a\u914d\u7f6e\uff0c\u4ee5JSON\u683c\u5f0f',
    'camera.select_type': '\u9009\u62e9\u7c7b\u578b...','camera.select_device': '\u9009\u62e9\u8bbe\u5907...','camera.loading_devices': '\u52a0\u8f7d\u4e2d...','camera.no_devices': '\u672a\u627e\u5230\u8bbe\u5907','camera.error_devices': '\u52a0\u8f7d\u8bbe\u5907\u5931\u8d25',
    'camera.error_name': '\u540d\u79f0\u4e0d\u80fd\u4e3a\u7a7a\u3002','camera.error_type': '\u7c7b\u578b\u4e0d\u80fd\u4e3a\u7a7a\u3002','camera.error_json': 'JSON\u683c\u5f0f\u65e0\u6548\u3002',
    'camera.device_label': 'USB\u8bbe\u5907','camera.use_as_camera': '\u4f5c\u4e3a\u6444\u50cf\u5934','camera.device_prefix': '\u8bbe\u5907 ',
    'camera.type_usb': 'USB','camera.type_rtsp': 'RTSP','camera.type_onvif': 'ONVIF','camera.type_gb28181': 'GB/T 28181','camera.type_rtmp': 'RTMP',
    'camera_view.title': '\u6444\u50cf\u5934','camera_view.loading': '\u52a0\u8f7d\u4e2d...','camera_view.load_failed': '\u52a0\u8f7d\u6444\u50cf\u5934\u5931\u8d25',
    'camera_view.no_stream': '\u65e0\u53ef\u7528\u6d41','camera_view.start_stream': '\u542f\u52a8\u6d41\u540e\u53ef\u67e5\u770b\u5b9e\u65f6\u9884\u89c8',
    'camera_view.url_copied': 'RTSP\u5730\u5740\u5df2\u590d\u5236','camera_view.url_copied_short': '\u94fe\u63a5\u5df2\u590d\u5236',
    'camera_view.local_capture': '\uff08\u672c\u5730\u91c7\u96c6\uff09','camera_view.back': '\u2190 \u8fd4\u56de',
    'caminfo.id': '\u7f16\u53f7','caminfo.type': '\u7c7b\u578b','caminfo.status': '\u72b6\u6001','caminfo.created': '\u521b\u5efa\u65f6\u95f4',
    'stream.started': '\u6d41\u5df2\u542f\u52a8','stream.stopped': '\u6d41\u5df2\u505c\u6b62',
    'stream.start': '\u542f\u52a8','stream.stop': '\u505c\u6b62','stream.view': '\u67e5\u770b','stream.edit': '\u7f16\u8f91','stream.delete': '\u5220\u9664','stream.copy': '\u590d\u5236',
    'settings.title': '\u8bbe\u7f6e','settings.subtitle': '\u914d\u7f6e\u7cfb\u7edf\u53c2\u6570','settings.save': '\u4fdd\u5b58\u8bbe\u7f6e','settings.saving': '\u4fdd\u5b58\u4e2d...',
    'settings.saved': '\u8bbe\u7f6e\u5df2\u4fdd\u5b58','settings.loading': '\u52a0\u8f7d\u8bbe\u7f6e...','settings.load_failed': '\u52a0\u8f7d\u8bbe\u7f6e\u5931\u8d25',
    'settings.server': '\u670d\u52a1\u5668','settings.web_port': 'Web UI\u7aef\u53e3','settings.rtsp_port': 'RTSP\u7aef\u53e3','settings.mibee': 'MiBee \u786c\u76d8\u5f55\u50cf\u673a',
    'settings.mibee_url': 'MiBee \u5730\u5740','settings.mibee_api_key': 'MiBee API \u5bc6\u94a5','settings.recording': '\u5f55\u5236',
    'settings.recordings_path': '\u5f55\u5236\u6587\u4ef6\u8def\u5f84','settings.max_segment_duration': '\u6700\u5927\u6bb5\u65f6\u957f\uff08\u79d2\uff09',
    'settings.change_password': '\u4fee\u6539\u5bc6\u7801','settings.current_password': '\u5f53\u524d\u5bc6\u7801','settings.new_password': '\u65b0\u5bc6\u7801','settings.confirm_new_password': '\u786e\u8ba4\u65b0\u5bc6\u7801',
    'settings.change_password_btn': '\u4fee\u6539\u5bc6\u7801','settings.changing': '\u4fee\u6539\u4e2d...','settings.password_changed': '\u5bc6\u7801\u5df2\u4fee\u6539\uff0c\u8bf7\u91cd\u65b0\u767b\u5f55\u3002',
    'settings.password_mismatch': '\u5bc6\u7801\u4e0d\u5339\u914d\u3002','settings.password_length': '\u5bc6\u7801\u81f3\u5c11\u9700\u89818\u4e2a\u5b57\u7b26\u3002','settings.fields_required': '\u6240\u6709\u5b57\u6bb5\u5747\u4e3a\u5fc5\u586b\u3002',
    'protocol.onvif': 'ONVIF \u914d\u7f6e','protocol.gb28181': 'GB28181 \u914d\u7f6e','protocol.rtmp': 'RTMP \u63a8\u6d41\u914d\u7f6e',
    'protocol.enabled': '\u542f\u7528','protocol.port': '\u7aef\u53e3','protocol.device_name': '\u8bbe\u5907\u540d\u79f0','protocol.device_id': '\u8bbe\u5907\u7f16\u53f7',
    'protocol.server_ip': '\u670d\u52a1\u5668IP','protocol.server_port': '\u670d\u52a1\u5668\u7aef\u53e3','protocol.url': '\u63a8\u6d41\u5730\u5740',
    'protocol.save': '\u4fdd\u5b58{name}','protocol.saved': '{name}\u914d\u7f6e\u5df2\u4fdd\u5b58','protocol.yes': '\u662f','protocol.no': '\u5426',
    'devices.title': '\u8bbe\u5907','devices.subtitle': '\u672c\u5730\u89c6\u9891\u548c\u97f3\u9891\u8bbe\u5907','devices.video': '\u89c6\u9891\u8bbe\u5907','devices.audio': '\u97f3\u9891\u8bbe\u5907',
    'devices.no_video': '\u672a\u627e\u5230\u89c6\u9891\u8bbe\u5907\u3002','devices.no_audio': '\u672a\u627e\u5230\u97f3\u9891\u8bbe\u5907\u3002','devices.load_failed': '\u52a0\u8f7d\u8bbe\u5907\u5931\u8d25',
    'devices.index': '\u7f16\u53f7: ','devices.formats': '\u683c\u5f0f: ','devices.configs': '\u914d\u7f6e: ',
    'common.cancel': '\u53d6\u6d88','common.delete': '\u5220\u9664','common.copy': '\u590d\u5236','common.loading': '\u52a0\u8f7d\u4e2d...','common.error': '\u9519\u8bef',
    'session.expired': '\u4f1a\u8bdd\u5df2\u8fc7\u671f',
    'usb.name': 'USB\u6444\u50cf\u5934 {index}','usb.created': 'USB\u6444\u50cf\u5934\u5df2\u521b\u5efa','usb.create_failed': '\u521b\u5efa\u6444\u50cf\u5934\u5931\u8d25',
    'error.network': '\u65e0\u6cd5\u8fde\u63a5\u670d\u52a1\u5668\u3002\u8bf7\u68c0\u67e5\u670d\u52a1\u662f\u5426\u8fd0\u884c\u5e76\u5237\u65b0\u3002',
    'error.failed': '\u5931\u8d25','error.failed_prefix': '\u5931\u8d25: ','error.unknown': '\u672a\u77e5',
    'login.username_ph': '用户名','login.password_ph': '密码',
    'setup.username_ph': 'admin','setup.password_ph': '强密码','setup.confirm_ph': '重复密码',
    'camera_view.ext_player': '使用外部RTSP播放器','camera_view.copy_rtsp': '复制RTSP地址',
    'camera.delete_title': '删除摄像头','camera.delete_desc': '确定删除吗？',
    'camera.name_ph': '前门','camera.config_ph': '{"url":"rtsp://192.168.1.100:554/stream1"}',
    'settings.web_port_ph': '9090','settings.rtsp_port_ph': '8554',
    'settings.mibee_url_ph': 'http://192.168.1.100:9090','settings.mibee_api_key_ph': '可选',
    'settings.recordings_path_ph': '/var/lib/mibee-rec/recordings','settings.max_segment_ph': '300',
    'settings.current_password_ph': '当前密码','settings.new_password_ph': '新密码（至少8个字符）','settings.confirm_new_password_ph': '重复新密码',
    'protocol.port_ph': '3702','protocol.device_name_ph': 'mibee-rec',
    'protocol.device_id_ph': '34020000001320000101','protocol.server_ip_ph': '192.168.1.100','protocol.server_port_ph': '5060','protocol.url_ph': 'rtmp://nvr-host:1935/live/stream',
    'protocol.save_onvif': '保存ONVIF','protocol.save_gb28181': '保存GB28181','protocol.save_rtmp': '保存RTMP',
    'theme.toggle': '\u5207\u6362\u4e3b\u9898','lang.toggle': '\u5207\u6362\u8bed\u8a00','lang.en': 'EN','lang.zh': '\u4e2d',
  }
};

// ============================================================
// STATE
// ============================================================
let S = { a: 0, c: [], s: {}, dc: null, urls: {}, lf: null };
// Toast queue for FIFO capping (F12)
let q = [];

// ============================================================
// API HELPER
// ============================================================
async function A(m, p, b) {
  let o = {
    method: m,
    headers: { 'Accept': 'application/json' },
    credentials: 'same-origin',
  };
  if (b !== undefined) {
    o.headers['Content-Type'] = 'application/json';
    o.body = JSON.stringify(b);
  }
  // CSRF: include X-CSRF-Token header on state-changing requests.
  // Token is set as a non-HttpOnly cookie on login.
  if (m === 'POST' || m === 'PUT' || m === 'DELETE' || m === 'PATCH') {
    let csrf = (document.cookie.match(/(?:^|; )csrf-token=([^;]+)/) || [])[1];
    if (csrf) o.headers['X-CSRF-Token'] = csrf;
  }
  let r = await fetch(p, o);

  // Global 401 handler (F10)
  if (r.status === 401 && S.a) {
    S.a = 0;
    T(t('session.expired'), 'i');
    setTimeout(() => { N(0); P('login'); window.location.hash = '#login'; }, 1500);
  }

  let d = null;
  if ((r.headers.get('content-type') || '').includes('application/json')) {
    d = await r.json();
  }
  return { ok: r.ok, status: r.status, data: d };
}

// ============================================================
// TOAST (F12: cap at 3, FIFO)
// ============================================================
function T(msg, t) {
  t = t || 'info';

  // Cap at 3, dismiss oldest (F12)
  while (q.length >= 3) {
    let o = q.shift();
    if (o && o.parentNode) o.remove();
  }

  let e = document.createElement('div');
  e.className = 'to-' + t;
  e.textContent = msg;
  document.getElementById('to').appendChild(e);
  q.push(e);

  setTimeout(() => {
    if (e.parentNode) {
      e.style.opacity = '0';
      e.style.transition = 'opacity 300ms ease';
      setTimeout(() => e.remove(), 300);
    }
    let i = q.indexOf(e);
    if (i !== -1) q.splice(i, 1);
  }, 4000);
}

// ============================================================
// NAVIGATION
// ============================================================
function G(h) { window.location.hash = h; }

function R() {
  let h = window.location.hash || '#db';
  let m = h.match(/^#\/c\/(.+)/);
  if (m) return { p: 'c', id: decodeURIComponent(m[1]) };
  let pg = h.replace(/^#\/?/, '') || 'db';
  return { p: pg, id: null };
}

// ============================================================
// PAGE SHOW/HIDE
// ============================================================
function P(id) {
  document.querySelectorAll('.pg').forEach(p => {
    p.classList.remove('a');
    p.classList.remove('ap');
  });
  let e = document.getElementById('pg-' + id);
  if (e) {
    e.classList.add('a');
    if (id === 'login' || id === 'stp') e.classList.add('ap');
  }
}

// ============================================================
// NAVBAR
// ============================================================
function N(s) { document.getElementById('nv').classList.toggle('h', !s); }

function U(pg) {
  document.querySelectorAll('.nl a').forEach(a =>
    a.classList.toggle('active', a.dataset.p === pg)
  );
}

// ============================================================
// AUTH CHECK
// ============================================================
async function C() {
  let r = await A('GET', '/api/cameras');
  if (r.status === 200)  { S.a = 1; return { a: 1, s: 0 }; }
  if (r.status === 503)  return { a: 0, s: 1 };
  return { a: 0, s: 0 };
}

// ============================================================
// LOGIN
// ============================================================
async function L(e) {
  e.preventDefault();
  let u = document.getElementById('lu').value.trim();
  let p = document.getElementById('lp').value;
  let el = document.getElementById('le');
  let sb = document.getElementById('ls');

  if (!u || !p) {
    el.textContent = t('login.error_empty');
    el.classList.add('s');
    return;
  }

  el.classList.remove('s');
  sb.disabled = 1;
  sb.textContent = t('login.signing_in');

  try {
    let r = await A('POST', '/api/auth/login', { username: u, password: p });
    if (r.ok) {
      T(t('login.signed_in'), 'success');
      await D();
    } else {
      el.textContent = r.data && r.data.error ? r.data.error : t('login.error_failed');
      el.classList.add('s');
    }
  } catch (_) {
    el.textContent = t('error.network');
    el.classList.add('s');
  } finally {
    sb.disabled = 0;
    sb.textContent = t('login.submit');
  }
}

// ============================================================
// SETUP (first-run)
// ============================================================
async function H(e) {
  e.preventDefault();
  let u = document.getElementById('su').value.trim();
  let p = document.getElementById('sp').value;
  let c = document.getElementById('sc').value;
  let el = document.getElementById('se');
  let sb = document.getElementById('ss');

  if (!u || !p) {
    el.textContent = t('setup.error_empty');
    el.classList.add('s');
    return;
  }

  if (p !== c) {
    el.textContent = t('setup.error_mismatch');
    el.classList.add('s');
    return;
  }

  // B1: backend requires >= 8
  if (p.length < 8) {
    el.textContent = t('setup.error_length');
    el.classList.add('s');
    return;
  }

  el.classList.remove('s');
  sb.disabled = 1;
  sb.textContent = t('setup.setting_up');

  try {
    let r = await A('POST', '/api/auth/setup', { username: u, password: p });
    if (r.ok) {
      T(t('setup.created'), 'success');
      P('login');
      document.getElementById('lu').value = u;
      document.getElementById('lp').value = '';
    } else {
      el.textContent = r.data && r.data.error ? r.data.error : t('setup.error_failed');
      el.classList.add('s');
    }
  } catch (_) {
    el.textContent = t('error.network');
    el.classList.add('s');
  } finally {
    sb.disabled = 0;
    sb.textContent = t('setup.submit');
  }
}

// ============================================================
// LOGOUT
// ============================================================
async function O() {
  try { await A('POST', '/api/auth/logout'); } catch (_) {}
  S.a = 0;
  S.c = [];
  N(0);
  P('login');
  window.location.hash = '#login';
  T(t('nav.signed_out'), 'info');
}

// ============================================================
// DASHBOARD — CAMERA LIST
// ============================================================
async function D() {
  P('db');
  N(1);
  U('db');

  document.getElementById('cl').classList.remove('h');
  document.getElementById('ce').classList.add('h');
  document.getElementById('ct').classList.add('h');

  try {
    let r = await A('GET', '/api/cameras');
    if (!r.ok) {
      // 401 now handled globally in A() (F10)
      T(t('error.failed_prefix') + (r.data && r.data.error || t('error.unknown')), 'e');
      document.getElementById('cl').classList.add('h');
      return;
    }
    S.c = Array.isArray(r.data) ? r.data : [];
    V();
    spoll();
  } catch (_) {
    T(t('error.network'), 'e');
    document.getElementById('cl').classList.add('h');
  }
}

function V() {
  document.getElementById('cl').classList.add('h');
  if (S.c.length === 0) {
    document.getElementById('ce').classList.remove('h');
    document.getElementById('ct').classList.add('h');
    return;
  }
  document.getElementById('ce').classList.add('h');
  document.getElementById('ct').classList.remove('h');
  let b = document.getElementById('cb');
  b.innerHTML = '';
  S.c.forEach(c => {
    let tr = document.createElement('tr');
    let bc = 'bg-' + (
      c.status === 'running' ? 'r' :
      c.status === 'stopped'  ? 'p' :
      'e'
    );
    let id = encodeURIComponent(c.id);
    tr.innerHTML =
      '<td data-label="' + t('table.name') + '"><a href="#/c/' + id + '" style=font-weight:500>' + E(c.name) + '</a></td>' +
      '<td data-label="' + t('table.type') + '"><span class=tm style="font-family:var(--mo);font-size:.857rem">' + E(F(c.camera_type)) + '</span></td>' +
      '<td data-label="' + t('table.status') + '"><span class="bg ' + bc + '">' + E(c.status) + '</span></td>' +
      '<td data-label="' + t('table.stream_url') + '">' + (
        c.status === 'running'
          ? '<div style="display:flex;align-items:center;gap:var(--s4)">' +
            '<code style="font-family:var(--mo);font-size:.786rem;color:var(--t2)">' +
            E(S.urls[c.id] || t('table.url_unavailable')) +
            '</code>' +
            '<button class="b bsm bs" data-url="' + J(S.urls[c.id] || '') + '" ' +
            'onclick="copyUrl(this.dataset.url)" aria-label="' + t('stream.copy') + '">' + t('stream.copy') + '</button></div>'
          : '-'
      ) + '</td>' +
      '<td data-label="' + t('table.actions') + '" class=ca>' +
        (c.status === 'running'
          ? '<button class="b bd bsm" onclick=w("' + id + '") aria-label="' + t('stream.stop') + '">' + t('stream.stop') + '</button>'
          : '<button class="b bp bsm" onclick=Y("' + id + '") aria-label="' + t('stream.start') + '">' + t('stream.start') + '</button>'
        ) +
        '<button class="b bs bsm" onclick=G("#/c/' + id + '") aria-label="' + t('stream.view') + '">' + t('stream.view') + '</button>' +
        '<button class="b bs bsm" onclick=ec(' + id + ') aria-label="' + t('stream.edit') + '">' + t('stream.edit') + '</button>' +
        '<button class="b bd bsm" onclick=x("' + id + '","' + J(c.name) + '") aria-label="' + t('stream.delete') + '">' + t('stream.delete') + '</button>' +
      '</td>';
    b.appendChild(tr);
  });
}
function F(ty) {
  return ({ usb: t('camera.type_usb'), rtsp: t('camera.type_rtsp'), onvif: t('camera.type_onvif'), gb28181: t('camera.type_gb28181'), rtmp: t('camera.type_rtmp') })[ty] || ty;
}

// ============================================================
// STREAM CONTROL
// ============================================================
async function Y(id) {
  let r = await A('POST', '/api/cameras/' + id + '/start');
  if (r.ok) {
    if (r.data && r.data.rtsp_url) {
      S.urls[r.data.camera_id || decodeURIComponent(id)] = r.data.rtsp_url;
    }
    T(t('stream.started'), 's');
    await D();
  } else {
    T(r.data && r.data.error ? r.data.error : t('error.failed'), 'e');
  }
}

async function w(id) {
  let r = await A('POST', '/api/cameras/' + id + '/stop');
  if (r.ok) {
    try { delete S.urls[decodeURIComponent(id)]; } catch (_) {}
    T(t('stream.stopped'), 's');
    await D();
  } else {
    T(r.data && r.data.error ? r.data.error : t('error.failed'), 'e');
  }
}

async function vY(id) {
  let r = await A('POST', '/api/cameras/' + id + '/start');
  if (r.ok) {
    if (r.data && r.data.rtsp_url) {
      S.urls[r.data.camera_id || decodeURIComponent(id)] = r.data.rtsp_url;
    }
    T(t('stream.started'), 's');
    await K(id);
  } else {
    T(r.data && r.data.error ? r.data.error : t('error.failed'), 'e');
  }
}


// ============================================================
// ADD CAMERA MODAL
// ============================================================
function wa() {
  S.editId = null;
  document.getElementById('mt').textContent = t('camera.add');
  document.getElementById('cn2').value = '';
  document.getElementById('cty').value = '';
  document.getElementById('cc').value = '';
  document.getElementById('me').classList.remove('s');
  document.getElementById('me').textContent = '';
  document.getElementById('msb').textContent = t('camera.add');

  let dg = document.getElementById('cam-device-group');
  if (dg) dg.classList.add('h');

  let ds = document.getElementById('cam-device-select');
  if (ds) {
    ds.innerHTML = '<option value="">' + t('camera.select_device') + '</option>';
    ds.disabled = true;
  }

  S.lf = document.activeElement;
  document.getElementById('mo').classList.remove('h');
  document.getElementById('cn2').focus();
}

function cm() { document.getElementById('mo').classList.add('h'); if (S.lf) { try { S.lf.focus(); } catch (_) {} S.lf = null; } }

async function sc() {
  let n = document.getElementById('cn2').value.trim();
  let t = document.getElementById('cty').value;
  let cfg = document.getElementById('cc').value.trim();
  let el = document.getElementById('me');
  let sb = document.getElementById('msb');

  if (!n)  { el.textContent = t('camera.error_name'); el.classList.add('s'); return; }
  if (!t)  { el.textContent = t('camera.error_type'); el.classList.add('s'); return; }

  let c = {};
  if (cfg) {
    try { c = JSON.parse(cfg); }
    catch (_) { el.textContent = t('camera.error_json'); el.classList.add('s'); return; }
  }

  let dg = document.getElementById('cam-device-group');
  let ds = document.getElementById('cam-device-select');
  if (dg && ds && !dg.classList.contains('h') && ds.value) {
    c.device_index = parseInt(ds.value);
  }

  el.classList.remove('s');
  sb.disabled = 1;
  sb.textContent = t('camera.saving');

  try {
    let r;
    if (S.editId) {
      r = await A('PUT', '/api/cameras/' + S.editId, { name: n, camera_type: t, config: c });
    } else {
      r = await A('POST', '/api/cameras', { name: n, camera_type: t, config: c });
    }
    if (r.ok) {
      T(S.editId ? t('camera.updated') : t('camera.added'), 's');
      delete S.editId;
      cm();
      await D();
    } else {
      el.textContent = r.data && r.data.error ? r.data.error : t('error.failed');
      el.classList.add('s');
    }
  } catch (_) {
    el.textContent = t('error.network');
    el.classList.add('s');
  } finally {
    sb.disabled = 0;
    sb.textContent = t('camera.add');
  }
}

// ============================================================
// DELETE CONFIRM
// ============================================================
function x(id, nm) {
  S.dc = decodeURIComponent(id);
  document.getElementById('ct2').textContent = t('camera.delete_confirm').replace('{name}', nm);
  S.lf = document.activeElement;
  document.getElementById('co').classList.remove('h');
  let fb = document.getElementById('cdb');
  if (fb) setTimeout(function() { fb.focus(); }, 50);
}

function cC() {
  document.getElementById('co').classList.add('h');
  if (S.lf) { try { S.lf.focus(); } catch (_) {} S.lf = null; }
  S.dc = null;
}

async function cD() {
  let id = S.dc;
  if (!id) return;

  let btn = document.getElementById('cdb');
  btn.disabled = 1;
  btn.textContent = t('camera.deleting');

  try {
    let r = await A('DELETE', '/api/cameras/' + id);
    if (r.ok) {
      T(t('camera.deleted'), 's');
      cC();
      await D();
    } else {
      T(r.data && r.data.error ? r.data.error : t('error.failed'), 'e');
      cC();
    }
  } catch (_) {
    T(t('error.network'), 'e');
    cC();
  } finally {
    btn.disabled = 0;
    btn.textContent = t('common.delete');
  }
}

// ============================================================
// FOCUS TRAP
// ============================================================
function ft(m, e) {
  let f = m.querySelectorAll('button,input,select,textarea,a[href],[tabindex]:not([tabindex="-1"])');
  if (f.length === 0) return;
  let fi = f[0], la = f[f.length - 1];
  if (e.shiftKey && document.activeElement === fi) { e.preventDefault(); la.focus(); }
  else if (!e.shiftKey && document.activeElement === la) { e.preventDefault(); fi.focus(); }
}

// ============================================================
// CAMERA LIVE VIEW
// ============================================================
async function K(id) {
  P('cam');
  N(1);
  U('');

  document.getElementById('cn').textContent = t('camera_view.loading');
  document.getElementById('cs').textContent = '';

  try {
    let r = await A('GET', '/api/cameras/' + id);
    if (!r.ok) {
      T(t('camera_view.load_failed'), 'e');
      G('#db');
      return;
    }
    let c = r.data;

    document.getElementById('cn').textContent = c.name;
    document.getElementById('cs').textContent = c.status;
    document.getElementById('ci-').textContent = c.id;
    document.getElementById('cit').textContent = F(c.camera_type);
    document.getElementById('cis').textContent = c.status;
    document.getElementById('cic').textContent = c.created_at || '-';
    document.getElementById('ru').textContent = c.config && c.config.url
      ? c.config.url
      : t('camera_view.local_capture');

    // Live preview: show MJPEG stream when running, placeholder otherwise.
    let pv = document.getElementById('pv');
    if (pv) {
      if (c.status === 'running') {
        pv.innerHTML = '<img src="/api/cameras/' + encodeURIComponent(c.id) + '/live" alt="Live preview" style="width:100%;height:100%;object-fit:contain;border-radius:var(--r);background:#000" onerror="if(this.parentNode)kE(\'' + encodeURIComponent(c.id) + '\')" />';
      } else {
        pv.innerHTML = '<div class=vi>&#x25B6;</div><h3>' + t('camera_view.no_stream') + '</h3><p class=tm>' + t('camera_view.start_stream') + '</p><button class="b bp" onclick=vY("' + encodeURIComponent(c.id) + '") style=margin-top:var(--s16) data-i18n="stream.start">' + t('stream.start') + '</button>';
      }
    }
  } catch (_) {
    T(t('error.network'), 'e');
    G('#db');
  }
}

// Fallback when MJPEG image fails to load (e.g. stream not active).
function kE(id) {
  let pv = document.getElementById('pv');
  if (pv) {
    pv.innerHTML = '<div class=vi>&#x25B6;</div><h3>' + t('camera_view.no_stream') + '</h3><p class=tm>' + t('camera_view.start_stream') + '</p><button class="b bp" onclick=vY("' + id + '") style=margin-top:var(--s16) data-i18n="stream.start">' + t('stream.start') + '</button>';
  }
}

function cr() {
  let u = document.getElementById('ru').textContent;
  if (!u) return;
  if (navigator.clipboard) {
    navigator.clipboard.writeText(u)
      .then(() => T(t('camera_view.url_copied'), 's'))
      .catch(() => fC(u));
  } else {
    fC(u);
  }
}

function fC(t) {
  let ta = document.createElement('textarea');
  ta.value = t;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  document.execCommand('copy');
  ta.remove();
  T(t('camera_view.url_copied'), 's');
}

function copyUrl(u) {
  if (navigator.clipboard) {
    navigator.clipboard.writeText(u)
      .then(() => T(t('camera_view.url_copied_short'), 's'))
      .catch(() => fC(u));
  } else {
    fC(u);
  }
}

// ============================================================
// DEVICE SELECTOR (USB device for camera creation)
// ============================================================
async function loadDeviceSelect() {
  let ds = document.getElementById('cam-device-select');
  if (!ds) return;
  ds.disabled = true;
  ds.innerHTML = '<option value="">' + t('camera.loading_devices') + '</option>';
  try {
    let r = await A('GET', '/api/devices/video');
    if (r.ok && Array.isArray(r.data)) {
      ds.innerHTML = '<option value="">' + t('camera.select_device') + '</option>';
      r.data.forEach(function(d, i) {
        let o = document.createElement('option');
        o.value = d.index !== undefined ? d.index : i;
        o.textContent = d.name || (t('camera.device_prefix') + o.value);
        ds.appendChild(o);
      });
      ds.disabled = false;
    } else {
      ds.innerHTML = '<option value="">' + t('camera.no_devices') + '</option>';
    }
  } catch (_) {
    ds.innerHTML = '<option value="">' + t('camera.error_devices') + '</option>';
  }
}

function setupDeviceSelector() {
  let s = document.getElementById('cty');
  let dg = document.getElementById('cam-device-group');
  if (!s || !dg) return;
  s.addEventListener('change', function() {
    if (this.value === 'usb') {
      dg.classList.remove('h');
      loadDeviceSelect();
    } else {
      dg.classList.add('h');
    }
  });
}

// ============================================================
// SETTINGS
// ============================================================
async function M() {
  P('st');
  N(1);
  U('st');

  document.getElementById('sl').classList.remove('h');
  document.getElementById('sfm').classList.add('h');

  try {
    let r = await A('GET', '/api/settings');
    if (!r.ok) {
      // 401 now handled globally in A() (F10)
      T(t('settings.load_failed'), 'e');
      document.getElementById('sl').classList.add('h');
      return;
    }
    S.s = r.data || {};

    let map = {
      web_port: 'swp', rtsp_port: 'srp',
      mibee_url: 'smu', mibee_api_key: 'smk',
      recordings_path: 'srp2', max_segment_duration: 'smd',
    };
    ['web_port', 'rtsp_port', 'mibee_url', 'mibee_api_key',
     'recordings_path', 'max_segment_duration'].forEach(k => {
      let el = document.getElementById(map[k]);
      if (el) el.value = S.s[k] || '';
    });

    document.getElementById('sl').classList.add('h');
    document.getElementById('sfm').classList.remove('h');
    lp();
  } catch (_) {
    T(t('error.network'), 'e');
    document.getElementById('sl').classList.add('h');
  }
}

async function Q() {
  let s = {};

  let map = {
    web_port: 'swp', rtsp_port: 'srp',
    mibee_url: 'smu', mibee_api_key: 'smk',
    recordings_path: 'srp2', max_segment_duration: 'smd',
  };
  ['web_port', 'rtsp_port', 'mibee_url', 'mibee_api_key',
   'recordings_path', 'max_segment_duration'].forEach(k => {
    let el = document.getElementById(map[k]);
    if (el) {
      let v = el.value.trim();
      if (v) s[k] = v;
    }
  });

  // F13: disable button while in-flight
  let btn = document.getElementById('sb');
  btn.disabled = 1;
  btn.textContent = t('settings.saving');

  try {
    let r = await A('PUT', '/api/settings', { settings: s });
    if (r.ok) {
      T(t('settings.saved'), 's');
    } else {
      T(r.data && r.data.error ? r.data.error : t('error.failed'), 'e');
    }
  } catch (_) {
    T(t('error.network'), 'e');
  } finally {
    btn.disabled = 0;
    btn.textContent = t('settings.save');
  }
}

// ============================================================
// DEVICES PAGE (Video/Audio discovery)
// ============================================================
async function loadDevices() {
  try {
    let vr = await A('GET', '/api/devices/video');
    let ar = await A('GET', '/api/devices/audio');

    let vd = document.getElementById('video-devices');
    let ad = document.getElementById('audio-devices');

    // Video devices
    let vh = '<h3 style="margin-bottom:var(--s16)">' + t('devices.video') + '</h3>';
    if (vr.ok && Array.isArray(vr.data) && vr.data.length > 0) {
      vr.data.forEach(function(d) {
        vh += '<div class="device-card">' +
              '<div class="device-name">' + E(d.name) + '</div>' +
              '<div class="device-info">Index: ' + d.index + '</div>';
        if (d.formats && d.formats.length > 0) {
          vh += '<div class="device-formats">' + t('devices.formats') + E(d.formats.join(', ')) + '</div>';
        }
        vh += '<button class="b bp bsm" onclick="createCameraFromDevice(' + d.index + ')">' + t('camera.use_as_camera') + '</button>' +
              '</div>';
      });
    } else {
      vh += '<p class="tm">' + t('devices.no_video') + '</p>';
    }
    vd.innerHTML = vh;

    // Audio devices
    let ah = '<h3 style="margin-bottom:var(--s16)">' + t('devices.audio') + '</h3>';
    if (ar.ok && Array.isArray(ar.data) && ar.data.length > 0) {
      ar.data.forEach(function(d) {
        ah += '<div class="device-card">' +
              '<div class="device-name">' + E(d.name) + '</div>';
        if (d.supported_configs && d.supported_configs.length > 0) {
          ah += '<div class="device-info">' + t('devices.configs') + E(d.supported_configs.join(', ')) + '</div>';
        }
        ah += '</div>';
      });
    } else {
      ah += '<p class="tm">' + t('devices.no_audio') + '</p>';
    }
    ad.innerHTML = ah;
  } catch (_) {
    T(t('devices.load_failed'), 'e');
  }
}

async function createCameraFromDevice(deviceIndex) {
  try {
    let r = await A('POST', '/api/cameras', {
      name: t('usb.name').replace('{index}', deviceIndex),
      camera_type: 'usb',
      config: { device_index: deviceIndex },
    });
    if (r.ok) {
      T(t('usb.created'), 's');
      await D();
    } else {
      T(r.data && r.data.error ? r.data.error : t('usb.create_failed'), 'e');
    }
  } catch (_) {
    T(t('error.network'), 'e');
  }
}

// ============================================================
// ROUTER
// ============================================================
async function r() {
  spoll_stop();
  let route = R();

  switch (route.p) {
    case 'login':
      N(0);
      P('login');
      break;

    case 'stp':
      N(0);
      P('stp');
      break;

    case 'c':
      if (!S.a) { await i(); return; }
      await K(route.id);
      break;

    case 'st':
      if (!S.a) { await i(); return; }
      await M();
      break;

    case 'dv':
      if (!S.a) { await i(); return; }
      P('dv');
      N(1);
      await loadDevices();
      break;

    default:
      if (!S.a) { await i(); return; }
      await D();
      break;
  }
}

// ============================================================
// INIT
// ============================================================
async function i() {
  let s = await C();

  if (s.a) {
    S.a = 1;
    N(1);
    await D();
  } else if (s.s) {
    S.a = 0;
    N(0);
    P('stp');
    window.location.hash = '#stp';
  } else {
    S.a = 0;
    N(0);
    P('login');
    window.location.hash = '#login';
  }
}

// ============================================================
// UTILITY
// ============================================================
function E(s) {
  if (typeof s !== 'string') return '';
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function J(s) {
  if (typeof s !== 'string') return '';
  return s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

// ============================================================

// ============================================================
// PASSWORD CHANGE (F6)
// ============================================================
async function cp() {
  let o = document.getElementById('cpw').value;
  let n = document.getElementById('npw').value;
  let c = document.getElementById('cnpw').value;
  if (!o || !n || !c) { T(t('settings.fields_required'), 'e'); return; }
  if (n !== c) { T(t('settings.password_mismatch'), 'e'); return; }
  if (n.length < 8) { T(t('settings.password_length'), 'e'); return; }
  let btn = document.getElementById('cpbtn');
  btn.disabled = 1;
  btn.textContent = t('settings.changing');
  try {
    let r = await A('POST', '/api/auth/reset', { old_password: o, new_password: n });
    if (r.ok) {
      T(t('settings.password_changed'), 's');
      setTimeout(() => { O(); }, 1500);
    } else {
      T(r.data && r.data.error ? r.data.error : t('error.failed'), 'e');
    }
  } catch (_) {
    T(t('error.network'), 'e');
  } finally {
    btn.disabled = 0;
    btn.textContent = t('settings.change_password_btn');
  }
}

// ============================================================
// CAMERA EDIT (F7)
// ============================================================
async function ec(id) {
  try {
    let r = await A('GET', '/api/cameras/' + id);
    if (!r.ok) { T(t('camera_view.load_failed'), 'e'); return; }
    let c = r.data;
    S.editId = id;
    document.getElementById('mt').textContent = t('camera.edit');
    document.getElementById('cn2').value = c.name || '';
    document.getElementById('cty').value = c.camera_type || '';
    document.getElementById('cc').value = c.config ? JSON.stringify(c.config, null, 2) : '';
    document.getElementById('mo').classList.remove('h');
  } catch (_) {
    T(t('error.network'), 'e');
  }
}

// ============================================================
// PROTOCOL CONFIG (F8)
// ============================================================
async function lp() {
  ['onvif', 'gb28181', 'rtmp'].forEach(async function(p) {
    try {
      let r = await A('GET', '/api/protocols/' + p);
      if (r.ok && r.data) {
        let prefix = { onvif: 'onv-', gb28181: 'gb-', rtmp: 'rtmp-' }[p];
        let map = { enabled: 'enabled', port: 'port', device_name: 'device-name',
                    manufacturer: 'manufacturer', model: 'model', device_id: 'device-id',
                    server_ip: 'server-ip', server_port: 'server-port', url: 'url' };
        Object.keys(r.data).forEach(function(k) {
          let el = document.getElementById(prefix + (map[k] || k));
          if (el) el.value = r.data[k];
        });
      }
    } catch (_) {}
  });
}

async function sp(p) {
  let prefix = { onvif: 'onv-', gb28181: 'gb-', rtmp: 'rtmp-' }[p];
  let cfg = {};
  let map = { enabled: 'enabled', port: 'port', 'device-name': 'device_name',
              'device-id': 'device_id', 'server-ip': 'server_ip',
              'server-port': 'server_port', 'url': 'url' };
  document.querySelectorAll('[id^="' + prefix + '"]').forEach(function(el) {
    let key = map[el.id.slice(prefix.length)] || el.id.slice(prefix.length);
    cfg[key] = el.value;
  });
  try {
    let r = await A('PUT', '/api/protocols/' + p, cfg);
    if (r.ok) T(t('protocol.saved').replace('{name}', p.toUpperCase()), 's');
    else T(r.data && r.data.error ? r.data.error : t('error.failed'), 'e');
  } catch (_) {
    T(t('error.network'), 'e');
  }
}

// ============================================================
// AUTO-REFRESH DASHBOARD (F11)
// ============================================================
function spoll() {
  if (pi) clearInterval(pi);
  pi = setInterval(async function() {
    if (window.location.hash !== '#db') { spoll_stop(); return; }
    try {
      let r = await A('GET', '/api/cameras');
      if (r.ok && Array.isArray(r.data)) {
        S.c = r.data;
        V();
      }
    } catch (_) {}
  }, 10000);
}

function spoll_stop() { if (pi) { clearInterval(pi); pi = null; } }

// EVENT BINDING
// ============================================================
document.addEventListener('DOMContentLoaded', function() {
  document.getElementById('lf').addEventListener('submit', L);
  document.getElementById('sf').addEventListener('submit', H);
  document.getElementById('lo').addEventListener('click', O);

  // Theme/language toggles
  let tt = document.getElementById('theme-toggle');
  if (tt) tt.addEventListener('click', toggleTheme);
  let lt = document.getElementById('lang-toggle');
  if (lt) lt.addEventListener('click', toggleLang);

  document.getElementById('mo').addEventListener('click', function(e) {
    if (e.target === this) cm();
  });
  document.getElementById('co').addEventListener('click', function(e) {
    if (e.target === this) cC();
  });

  document.addEventListener('keydown', function(e) {
    let mo = document.getElementById('mo'), co = document.getElementById('co');
    if (e.key === 'Escape') {
      if (!mo.classList.contains('h')) cm();
      if (!co.classList.contains('h')) cC();
    }
    if (e.key === 'Tab') {
      if (!mo.classList.contains('h')) ft(mo, e);
      else if (!co.classList.contains('h')) ft(co, e);
    }
  });

  document.getElementById('hbtn').addEventListener('click', function() {
    document.querySelector('.nl').classList.toggle('open');
  });
  document.querySelectorAll('.nl a').forEach(function(a) {
    a.addEventListener('click', function() {
      document.querySelector('.nl').classList.remove('open');
    });
  });

  window.addEventListener('hashchange', r);
  setupDeviceSelector();
  initThemeAndLang();
  i();
});

// Expose functions for onclick handlers in HTML
window.G = G;
window.wa = wa;
window.cm = cm;
window.sc = sc;
window.x = x;
window.cC = cC;
window.cD = cD;
window.Y = Y;
window.w = w;
window.cr = cr;
window.Q = Q;
window.A = A;
window.copyUrl = copyUrl;
window.createCameraFromDevice = createCameraFromDevice;
window.cp = cp;
window.ec = ec;
window.sp = sp;
window.toggleTheme = toggleTheme;
window.toggleLang = toggleLang;
window.translatePage = translatePage;
window.vY = vY;

})();
