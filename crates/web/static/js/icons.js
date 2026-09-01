// Icon system — the single source for every icon in the UI.
//
// Hand-drawn 24×24 stroke icons (1.7px, round caps) so the zero-build,
// zero-dependency constraint holds. Static markup references them via
// <i data-icon="name" data-size="18"></i> placeholders that initIcons()
// swaps for real SVGs at boot; JS-generated DOM calls icon() directly.

const PATHS = {
  // Brand lens mark (also inlined in index.html for first paint).
  logo: '<circle cx="12" cy="12" r="8.2"/><circle cx="12" cy="12" r="3"/><path d="M12 1.6v3M12 19.4v3M1.6 12h3M19.4 12h3"/>',
  live: '<rect x="2.5" y="5" width="19" height="14" rx="2.5"/><path d="M10 9.2v5.6l4.8-2.8z" fill="currentColor" stroke="none"/>',
  cameras: '<rect x="3.5" y="3.5" width="7" height="7" rx="2"/><rect x="13.5" y="3.5" width="7" height="7" rx="2"/><rect x="3.5" y="13.5" width="7" height="7" rx="2"/><rect x="13.5" y="13.5" width="7" height="7" rx="2"/>',
  settings: '<path d="M4 7h7.5M16.5 7H20M4 17h3.5M12.5 17H20"/><circle cx="14" cy="7" r="2.3"/><circle cx="10" cy="17" r="2.3"/>',
  status: '<path d="M3 12h3.5L9 5.5l4.5 13L16 12h5"/>',
  devices: '<rect x="7" y="7" width="10" height="10" rx="2"/><path d="M10 10.5h4M10 13.5h4M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.2 5.2l2 2M16.8 16.8l2 2M18.8 5.2l-2 2M7.2 16.8l-2 2"/>',
  globe: '<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.6 2.3 4 5.2 4 8.5s-1.4 6.2-4 8.5c-2.6-2.3-4-5.2-4-8.5s1.4-6.2 4-8.5z"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5 5l1.4 1.4M17.6 17.6 19 19M19 5l-1.4 1.4M6.4 17.6 5 19"/>',
  moon: '<path d="M20.6 13.2A8.5 8.5 0 1 1 10.8 3.4a6.6 6.6 0 0 0 9.8 9.8z"/>',
  logout: '<path d="M13.5 4.5H18A1.5 1.5 0 0 1 19.5 6v12a1.5 1.5 0 0 1-1.5 1.5h-4.5"/><path d="M4 12h11M7.5 8.5 4 12l3.5 3.5"/>',
  camera: '<path d="M3.5 8.5A1.5 1.5 0 0 1 5 7h2.6l1.6-2.2A1.5 1.5 0 0 1 10.4 4h3.2a1.5 1.5 0 0 1 1.2.6L16.4 7H19a1.5 1.5 0 0 1 1.5 1.5V17a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 17z"/><circle cx="12" cy="12.5" r="3.4"/>',
  expand: '<path d="M4 9.5v-4A1.5 1.5 0 0 1 5.5 4h4M14.5 4h4A1.5 1.5 0 0 1 20 5.5v4M20 14.5v4a1.5 1.5 0 0 1-1.5 1.5h-4M9.5 20h-4A1.5 1.5 0 0 1 4 18.5v-4"/>',
  compress: '<path d="M9.5 4v4A1.5 1.5 0 0 1 8 9.5h-4M20 9.5h-4A1.5 1.5 0 0 1 14.5 8V4M14.5 20v-4a1.5 1.5 0 0 1 1.5-1.5h4M4 14.5h4A1.5 1.5 0 0 1 9.5 16v4"/>',
  'flip-h': '<path d="M12 3.5v17M8.5 8 4.5 12l4 4M15.5 8l4 4-4 4"/>',
  'flip-v': '<path d="M3.5 12h17M8 8.5 12 4.5l4 4M8 15.5l4 4 4-4"/>',
  refresh: '<path d="M4.5 12a7.5 7.5 0 0 1 12.9-5.2L20 9.2"/><path d="M20 4.2v5h-5"/><path d="M19.5 12a7.5 7.5 0 0 1-12.9 5.2L4 14.8"/><path d="M4 19.8v-5h5"/>',
  play: '<path d="M8 5.5v13l10-6.5z" fill="currentColor" stroke="none"/>',
  stop: '<rect x="7" y="7" width="10" height="10" rx="1.5" fill="currentColor" stroke="none"/>',
  record: '<circle cx="12" cy="12" r="6.5"/><circle cx="12" cy="12" r="2.6" fill="currentColor" stroke="none"/>',
  trash: '<path d="M4.5 6.5h15M9.5 6.2V5a1.5 1.5 0 0 1 1.5-1.5h2A1.5 1.5 0 0 1 14.5 5v1.2M6.3 6.5l.8 12.2a1.5 1.5 0 0 0 1.5 1.3h6.8a1.5 1.5 0 0 0 1.5-1.3l.8-12.2"/><path d="M10 10.5v5.5M14 10.5v5.5"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  check: '<path d="M4.5 12.5 10 18 19.5 6.5"/>',
  x: '<path d="M6 6l12 12M18 6 6 18"/>',
  alert: '<path d="M12 3.5 2.8 19.5h18.4z"/><path d="M12 10v4.2M12 17.2v.2"/>',
  info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11.2v5M12 7.8v.2"/>',
  'chevron-down': '<path d="M6 9.5l6 6 6-6"/>',
  eye: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>',
  'eye-off': '<path d="M4 4l16 16"/><path d="M10.4 5.8A9.7 9.7 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a17 17 0 0 1-2.7 3.6M6.6 6.7A16 16 0 0 0 2.5 12S6 18.5 12 18.5c1.2 0 2.3-.2 3.3-.6"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/>',
  video: '<rect x="3" y="6.5" width="13" height="11" rx="2"/><path d="M16 10.5l5-3v9l-5-3"/>',
  mic: '<rect x="9.5" y="3.5" width="5" height="10" rx="2.5"/><path d="M6 11.5a6 6 0 0 0 12 0M12 17.5v3"/>',
  home: '<path d="M4 11.5 12 5l8 6.5M6 10v9.5h12V10"/>',
  up: '<path d="M12 19V5M5 12l7-7 7 7"/>',
  down: '<path d="M12 5v14M5 12l7 7 7-7"/>',
  left: '<path d="M19 12H5M12 19l-7-7 7-7"/>',
  right: '<path d="M5 12h14M12 5l7 7-7 7"/>',
  off: '<rect x="3.5" y="6.5" width="13" height="11" rx="2"/><path d="M16 10.5l4.5-2.7v8.4L16 13.5M3 3.5l18 17"/>',
};

export function icon(name, size = 18) {
  const body = PATHS[name];
  if (!body) return '';
  return '<svg class="icon" viewBox="0 0 24 24" width="' + size + '" height="' + size + '"' +
    ' fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"' +
    ' stroke-linejoin="round" aria-hidden="true">' + body + '</svg>';
}

/// Replace every <i data-icon="…"> placeholder under root with its SVG.
/// Classes on the placeholder are carried over (theme.js relies on
/// .icon-sun/.icon-moon toggling).
export function initIcons(root = document) {
  root.querySelectorAll('i[data-icon]').forEach((node) => {
    const svg = icon(node.dataset.icon, Number(node.dataset.size) || 18);
    if (!svg) return;
    const holder = document.createElement('span');
    holder.innerHTML = svg;
    const el = holder.firstChild;
    if (node.className) el.setAttribute('class', 'icon ' + node.className);
    node.replaceWith(el);
  });
}
