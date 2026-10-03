// Short notices in the corner, and confirmation before destructive actions.

export type Tone = 'info' | 'ok' | 'warn' | 'danger';
export interface Toast {
  id: number;
  tone: Tone;
  message: string;
}
export const toasts = $state<Toast[]>([]);
let next = 1;

export function notify(message: string, tone: Tone = 'info', ms = tone === 'danger' ? 8000 : 4000) {
  const id = next++;
  toasts.push({ id, tone, message });
  setTimeout(() => dismiss(id), ms);
}
export function dismiss(id: number) {
  const index = toasts.findIndex((toast) => toast.id === id);
  if (index >= 0) toasts.splice(index, 1);
}
/** Report a failed action with the server's message. */
export function failed(action: string, error: unknown) {
  const reason = error instanceof Error ? error.message : String(error);
  notify(`${action}: ${reason}`, 'danger');
}

export interface Confirmation {
  title: string;
  message: string;
  confirm: string;
  danger: boolean;
  resolve: (value: boolean) => void;
}
export const dialog = $state<{ current: Confirmation | null }>({ current: null });

export function confirm(options: { title: string; message: string; confirm?: string; danger?: boolean }): Promise<boolean> {
  return new Promise((resolve) => {
    dialog.current = {
      title: options.title,
      message: options.message,
      confirm: options.confirm ?? 'Continue',
      danger: options.danger ?? false,
      resolve: (value) => {
        dialog.current = null;
        resolve(value);
      },
    };
  });
}
