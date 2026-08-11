/** ui/src/lib/globalErrors.test.ts — 任务 17 全局错误屏显与持久日志回归（此前零覆盖）。
 *  messageOf / persist 为模块私有函数，一律经公共行为（reportError、window 事件、
 *  toast DOM、localStorage）间接验证；installGlobalErrorHandlers 的模块级 installed
 *  标志通过 vi.resetModules() + 动态 import 重置。 */

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import i18n from '../i18n';
import { reportError } from './globalErrors';

const STORAGE_KEY = 'ab.error-log';
const TOAST_SELECTOR = '[data-testid="global-error-toast"]';

function toastText(): string {
  return document.querySelector(TOAST_SELECTOR)?.textContent ?? '';
}

function storedLog(): Array<Record<string, unknown>> {
  const raw = localStorage.getItem(STORAGE_KEY);
  return raw ? (JSON.parse(raw) as Array<Record<string, unknown>>) : [];
}

beforeEach(() => {
  // 全局错误处理函数本就会打印 console.error，测试中静默。
  vi.spyOn(console, 'error').mockImplementation(() => undefined);
});

afterEach(() => {
  document.body.innerHTML = '';
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('task 17: messageOf（经 reportError 公共行为）', () => {
  it('Error 对象 → 取 message', () => {
    reportError(new Error('boom: fetch failed'));
    expect(toastText()).toContain('boom: fetch failed');
  });

  it('message 为空的 Error → 回落 name', () => {
    reportError(new Error(''));
    expect(toastText()).toContain('Error');
  });

  it('字符串值 → 原样透传', () => {
    reportError('plain failure');
    expect(toastText()).toContain('plain failure');
  });

  it('普通对象 → JSON 序列化', () => {
    reportError({ code: 42, msg: 'bad' });
    expect(toastText()).toContain('{"code":42,"msg":"bad"}');
  });

  it('循环引用对象 → 不抛错，String 兜底', () => {
    const circular: { self?: unknown } = {};
    circular.self = circular;
    expect(() => reportError(circular)).not.toThrow();
    expect(toastText()).toContain('[object Object]');
  });
});

describe('task 17: persist（经 reportError 写入 localStorage）', () => {
  it('以 JSON 数组写入 ab.error-log，含 kind/message/source', () => {
    reportError('boom', 'query_series');
    expect(storedLog()).toEqual([
      expect.objectContaining({ kind: 'error', message: 'boom', source: 'query_series' }),
    ]);
  });

  it('最多保留最近 20 条（slice(-20)，新条目覆盖旧条目）', () => {
    for (let i = 0; i < 25; i++) reportError(`err-${i}`, 'src');
    const log = storedLog();
    expect(log).toHaveLength(20);
    expect(log[0].message).toBe('err-5');
    expect(log[19].message).toBe('err-24');
  });

  it('存储值损坏不崩溃：null 与 {} 回落空数组继续记录，非法 JSON 静默跳过', () => {
    for (const raw of ['null', '{}']) {
      localStorage.setItem(STORAGE_KEY, raw);
      expect(() => reportError('fresh', 'src')).not.toThrow();
      expect(storedLog()).toEqual([
        expect.objectContaining({ kind: 'error', message: 'fresh', source: 'src' }),
      ]);
    }

    localStorage.setItem(STORAGE_KEY, 'not-json');
    expect(() => reportError('after-corrupt', 'src')).not.toThrow();
  });
});

describe('task 17: reportError', () => {
  it('console.error 带 [ab] 前缀、source 与原始值', () => {
    const err = new Error('boom');
    reportError(err, 'query_series');
    expect(console.error).toHaveBeenCalledWith('[ab]', 'query_series', 'boom', err);
  });

  it('未传 source → 占位 "error"', () => {
    reportError('plain');
    expect(console.error).toHaveBeenCalledWith('[ab]', 'error', 'plain', 'plain');
  });

  it('toast 含 i18n 标签 + [source] 前缀 + 消息（role=alert）', () => {
    reportError('boom', 'query_series');
    const toast = document.querySelector(TOAST_SELECTOR);
    expect(toast).not.toBeNull();
    expect(toast!.getAttribute('role')).toBe('alert');
    expect(toast!.textContent).toContain(i18n.t('errors.global.script'));
    expect(toast!.textContent).toContain('[query_series]');
    expect(toast!.textContent).toContain('boom');
  });

  it('多次上报 → 宿主 #ab-global-errors 仅一个，toast 各自独立', () => {
    reportError('a');
    reportError('b');
    expect(document.querySelectorAll('#ab-global-errors')).toHaveLength(1);
    expect(document.querySelectorAll(TOAST_SELECTOR)).toHaveLength(2);
    expect(document.getElementById('ab-global-errors')?.children).toHaveLength(2);
  });

  it('toast 8000ms 后自动移除，宿主保留', () => {
    vi.useFakeTimers();
    reportError('boom');
    expect(document.querySelectorAll(TOAST_SELECTOR)).toHaveLength(1);
    vi.advanceTimersByTime(8000);
    expect(document.querySelectorAll(TOAST_SELECTOR)).toHaveLength(0);
    expect(document.getElementById('ab-global-errors')).not.toBeNull();
  });
});

describe('task 17: installGlobalErrorHandlers', () => {
  let loaded: typeof import('./globalErrors');

  beforeAll(async () => {
    // 重置模块注册表，让 installed 标志回到初始 false，且 listener 只注册一次。
    vi.resetModules();
    loaded = await import('./globalErrors');
  });

  it('重复安装幂等：只注册一组处理函数（一次事件 → 一个 toast）', () => {
    loaded.installGlobalErrorHandlers();
    loaded.installGlobalErrorHandlers();
    window.dispatchEvent(new ErrorEvent('error', { error: new Error('once') }));
    expect(document.querySelectorAll(TOAST_SELECTOR)).toHaveLength(1);
    expect(storedLog()).toHaveLength(1);
  });

  it('window error 事件 → toast + 持久化 kind=error（取 event.error ?? event.message）', () => {
    loaded.installGlobalErrorHandlers();
    window.dispatchEvent(
      new ErrorEvent('error', {
        error: new Error('fetch failed: 500'),
        message: 'fallback text',
        filename: 'http://app/main.js',
        lineno: 42,
      }),
    );
    expect(toastText()).toContain('fetch failed: 500');
    expect(storedLog()).toEqual([
      expect.objectContaining({
        kind: 'error',
        message: 'fetch failed: 500',
        source: 'http://app/main.js:42',
      }),
    ]);
  });

  it('window error 事件缺少 error 时回落 event.message', () => {
    loaded.installGlobalErrorHandlers();
    const event = new Event('error');
    Object.defineProperty(event, 'message', { value: 'window-level failure' });
    window.dispatchEvent(event);
    expect(toastText()).toContain('window-level failure');
    expect(storedLog()).toEqual([
      expect.objectContaining({ kind: 'error', message: 'window-level failure' }),
    ]);
  });

  it('window unhandledrejection 事件 → toast + 持久化 kind=unhandledrejection（取 event.reason）', () => {
    loaded.installGlobalErrorHandlers();
    const event = new Event('unhandledrejection');
    Object.defineProperty(event, 'reason', { value: new Error('db write failed') });
    window.dispatchEvent(event);
    expect(toastText()).toContain('db write failed');
    expect(storedLog()).toEqual([
      expect.objectContaining({ kind: 'unhandledrejection', message: 'db write failed' }),
    ]);
  });

  it('处理函数不抛错、不 preventDefault', () => {
    loaded.installGlobalErrorHandlers();
    const event = new Event('error');
    expect(() => window.dispatchEvent(event)).not.toThrow();
    expect(event.defaultPrevented).toBe(false);
  });
});
