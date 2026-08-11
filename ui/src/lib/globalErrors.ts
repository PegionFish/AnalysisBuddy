/** ui/src/lib/globalErrors.ts — 全局错误屏显与持久日志（任务 17 必做正式功能）。
 *  window.onerror / unhandledrejection 原本在 release 包中完全不可见（无 DevTools、
 *  stderr 不落盘）。这里：
 *  1) 右上角弹出自动消退的错误横幅（i18n 标签 + 原始错误文本透传）；
 *  2) 滚动保留最近 20 条到 localStorage `ab.error-log`，供事后取证。 */

import i18n from '../i18n';

const STORAGE_KEY = 'ab.error-log';
const MAX_ENTRIES = 20;
const TOAST_TTL_MS = 8000;

interface ErrorLogEntry {
  kind: 'error' | 'unhandledrejection';
  message: string;
  source?: string;
  ts_ms: number;
}

let installed = false;

function persist(kind: ErrorLogEntry['kind'], message: string, source?: string): void {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    const list: ErrorLogEntry[] = Array.isArray(parsed) ? parsed : [];
    list.push({ kind, message, source, ts_ms: Date.now() });
    localStorage.setItem(STORAGE_KEY, JSON.stringify(list.slice(-MAX_ENTRIES)));
  } catch {
    /* localStorage 不可用时仅屏显 */
  }
}

function messageOf(value: unknown): string {
  if (value instanceof Error) return value.message || value.name;
  if (typeof value === 'string') return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

/** 屏显横幅：不依赖 React 树（React 树可能已经卸载），直接挂 DOM。 */
function showToast(kind: ErrorLogEntry['kind'], message: string): void {
  try {
    let host = document.getElementById('ab-global-errors');
    if (!host) {
      host = document.createElement('div');
      host.id = 'ab-global-errors';
      host.style.cssText =
        'position:fixed;top:12px;right:12px;z-index:2147483647;display:flex;flex-direction:column;gap:8px;max-width:420px;';
      document.body.appendChild(host);
    }
    const label =
      kind === 'unhandledrejection' ? i18n.t('errors.global.unhandled') : i18n.t('errors.global.script');
    const toast = document.createElement('div');
    toast.setAttribute('role', 'alert');
    toast.setAttribute('data-testid', 'global-error-toast');
    toast.style.cssText =
      'padding:10px 14px;background:#3a1f1d;color:#ffb4ab;border:1px solid #b3453f;' +
      'border-radius:6px;font:12px/1.5 system-ui,sans-serif;white-space:pre-wrap;word-break:break-word;';
    toast.textContent = `${label}: ${message}`;
    host.appendChild(toast);
    window.setTimeout(() => toast.remove(), TOAST_TTL_MS);
  } catch {
    /* DOM 不可用时静默，持久日志仍在 */
  }
}

/** 应用代码主动上报错误（任务 21：禁止静默吞错）。
 *  与 window.onerror 同待遇：console.error + 持久日志 + 屏显横幅。
 *  source 建议传「逻辑位置」（如 'query_series'）便于取证。 */
export function reportError(message: unknown, source?: string): void {
  const text = messageOf(message);
  console.error('[ab]', source ?? 'error', text, message);
  persist('error', text, source);
  showToast('error', `${source ? `[${source}] ` : ''}${text}`);
}

/** 安装一次（main.tsx 启动时调用）。 */
export function installGlobalErrorHandlers(): void {
  if (installed) return;
  installed = true;

  window.addEventListener('error', (event) => {
    const message = messageOf(event.error ?? event.message);
    const source = event.filename ? `${event.filename}:${event.lineno}` : undefined;
    console.error('[global] error', message, source);
    persist('error', message, source);
    showToast('error', message);
  });

  window.addEventListener('unhandledrejection', (event) => {
    const message = messageOf(event.reason);
    console.error('[global] unhandledrejection', event.reason);
    persist('unhandledrejection', message);
    showToast('unhandledrejection', message);
  });
}
