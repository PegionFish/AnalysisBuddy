/** ui/src/ipc/real.ts — real Tauri IPC implementation (ipc-ui.md §3.4).
 *  This is the ONLY file allowed to import Tauri bindings (@tauri-apps/api,
 *  @tauri-apps/plugin-dialog) — enforced by ESLint no-restricted-imports. */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Ipc } from './ipc';
import type { IpcError } from './types';

/** OS 文件拖放事件通道（Tauri 2 core 事件，不属于 ab:// 契约，故不放入
 *  与 Rust 常量镜像的 events.ts）。生产壳层 Tauri 拦截 OS 拖放，HTML5 drop
 *  拿不到真实路径，必须由这三个事件驱动导入入口。 */
export const EV_OS_DRAG_ENTER = 'tauri://drag-enter';
export const EV_OS_DRAG_LEAVE = 'tauri://drag-leave';
export const EV_OS_DRAG_DROP = 'tauri://drag-drop';

/** `tauri://drag-drop` 载荷：paths 为 OS 层给出的被拖入文件绝对路径。 */
export interface OsDragDropPayload {
  paths: string[];
  position: { x: number; y: number };
}

/** Normalize rejected values (Tauri serialization boundary may yield strings) to IpcError. */
function normalizeError(e: unknown): IpcError {
  if (e && typeof e === 'object' && 'code' in e && typeof (e as { code: unknown }).code === 'string') {
    return e as IpcError;
  }
  return {
    code: 'internal',
    message: typeof e === 'string' ? e : 'internal error',
    data: e,
  };
}

/** Open the native multi-file picker (tauri-plugin-dialog; `dialog:default` ACL granted).
 *  Resolves to the picked absolute paths; an empty array when the user cancels.
 *  Plugin manifests expose no extension list to the UI, so no filters are applied. */
export async function pickImportFiles(): Promise<string[]> {
  // Dynamic import keeps the dialog chunk out of the mock/dev path.
  const { open } = await import('@tauri-apps/plugin-dialog');
  const picked = await open({ multiple: true });
  if (picked === null) return [];
  return Array.isArray(picked) ? picked : [picked];
}

/** 原生另存为对话框（任务 17 save_session 修复）：对话框改由前端发起——
 *  Rust 侧 save_file 回调在打包版存在不触发风险（oneshot await 永久挂起→
 *  无任何反馈）；前端 `save()` 与已验证可用的 `open()` 同源同 ACL。
 *  取消 → null；得到的路径以显式 `path` 传给 save_session 命令。 */
export async function pickSavePath(): Promise<string | null> {
  const { save } = await import('@tauri-apps/plugin-dialog');
  const picked = await save({
    filters: [{ name: 'AnalysisBuddy Session', extensions: ['absession'] }],
    defaultPath: 'session.absession',
    title: 'Save AnalysisBuddy Session',
  });
  return picked ?? null;
}

/** 原生模块 ZIP 选择对话框（spec §6.1）：单文件 + .zip 过滤；取消 → null。 */
export async function pickPluginZip(): Promise<string | null> {
  const { open } = await import('@tauri-apps/plugin-dialog');
  const picked = await open({
    multiple: false,
    filters: [{ name: 'Plugin ZIP', extensions: ['zip'] }],
    title: 'Install Plugin ZIP',
  });
  if (picked === null) return null;
  return Array.isArray(picked) ? (picked[0] ?? null) : picked;
}

export function createRealIpc(): Ipc {
  const call = <T, A>(command: string, args: A): Promise<T> => invoke<T>(command, args as never).catch((e: unknown) => {
    throw normalizeError(e);
  });

  return {
    list_plugins: () => call('list_plugins', {}),
    import_files: (args) => call('import_files', args),
    unload_file: (args) => call('unload_file', args),
    get_metrics: (args) => call('get_metrics', args),
    query_series: (args) => call('query_series', args),
    key_values_at: (args) => call('key_values_at', args),
    save_session: (args) => call('save_session', args),
    load_session: (args) => call('load_session', args),
    get_plugin_log: (args) => call('get_plugin_log', args),
    reload_plugin: (args) => call('reload_plugin', args),
    install_plugin_zip: (args) => call('install_plugin_zip', args),
    uninstall_plugin: (args) => call('uninstall_plugin', args),
    set_plugin_enabled: (args) => call('set_plugin_enabled', args),
    check_plugin_update: (args) => call('check_plugin_update', args),
    update_plugin: (args) => call('update_plugin', args),
    pickSavePath: () => pickSavePath(),
    listen<T>(channel: string, cb: (payload: T) => void) {
      let unlisten: (() => void) | null = null;
      let disposed = false;
      listen<T>(channel, (event) => cb(event.payload)).then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      });
      return () => {
        disposed = true;
        unlisten?.();
      };
    },
  };
}
