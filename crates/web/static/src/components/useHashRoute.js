// Hash-based router hook. Returns the current route token parsed from
// window.location.hash, re-rendering on hashchange.
//
// Supported routes:
//   '' or '#db'        → dashboard (camera list + live grid)
//   '#/c/{id}'          → single camera detail
//   '#st'               → settings
//   '#dv'               → devices
//   '#login' / '#setup' → auth screens
import { useEffect, useState } from 'preact/hooks';

export function useHashRoute() {
  const [hash, setHash] = useState(() => window.location.hash);
  useEffect(() => {
    const onChange = () => setHash(window.location.hash);
    window.addEventListener('hashchange', onChange);
    return () => window.removeEventListener('hashchange', onChange);
  }, []);
  return hash;
}

/// Navigate to a route by setting window.location.hash.
export function navigate(route) {
  window.location.hash = route;
}

/// Match the current hash against `pattern` (with `{param}` placeholders).
/// Returns the captured params, or null if no match.
export function matchRoute(hash, pattern) {
  // Convert "/c/{id}" → /^#?\/c\/([^/]+)$/
  const re = new RegExp(
    '^#?' + pattern.replace(/[{}]/g, '').replace(/:[^/]+|\w+/g, (m) => {
      return pattern.includes('{' + m + '}') ? '([^/]+)' : m;
    }) + '$'
  );
  const m = hash.match(re);
  return m ? m.slice(1) : null;
}
