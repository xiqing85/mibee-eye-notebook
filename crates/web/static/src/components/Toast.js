// Toast notification viewport. FIFO-capped at 3 by the store.
import { h } from 'preact';
import { toasts, dismissToast } from '../store.js';
import { useStore } from '../hooks.js';

export function ToastViewport() {
  const items = useStore(toasts);
  return (
    <div class="to" role="status" aria-live="polite">
      {items.map((it) => (
        <div key={it.id} class={`toast toast-${it.type}`} onClick={() => dismissToast(it.id)}>
          {it.message}
        </div>
      ))}
    </div>
  );
}
