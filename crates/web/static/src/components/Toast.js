// Toast notification viewport. FIFO-capped at 3 by the store.
import { h } from 'preact';
import { toasts, dismissToast } from '../store.js';

export function ToastViewport() {
  const items = toasts.value;
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
