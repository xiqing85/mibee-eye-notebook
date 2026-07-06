// React-style hooks: re-export from preact/hooks plus a useStore adapter
// for our custom signal primitive.
//
// `useStore(sig)` subscribes a component to a signal and returns its current
// value, re-rendering the component whenever the signal mutates.
import { useEffect, useState, useRef, useCallback } from 'preact/hooks';

/// Subscribe to a signal; re-render on change. Returns the current value.
export function useStore(sig) {
  const [, setN] = useState(0);
  useEffect(() => {
    return sig.subscribe(() => setN((n) => n + 1));
  }, [sig]);
  return sig.value;
}

/// Like useEffect but the callback receives an AbortSignal that aborts on
/// cleanup/unmount — handy for fetch-in-effect patterns.
export function useAbortableEffect(fn, deps) {
  useEffect(() => {
    const ctrl = new AbortController();
    fn(ctrl.signal);
    return () => ctrl.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}

export { useEffect, useState, useRef, useCallback };
