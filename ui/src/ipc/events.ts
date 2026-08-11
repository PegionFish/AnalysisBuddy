/** ui/src/ipc/events.ts — event channel constants and payload types (ipc-ui.md §2).
 *  Channel strings are byte-identical to the Rust-side constants in ab-app. */

import type { PluginState } from './types';

export const EV_PROGRESS = 'ab://progress';
export const EV_PLUGIN_LOG = 'ab://plugin-log';
export const EV_PLUGIN_HEALTH = 'ab://plugin-health';
/** 插件发现重扫完成（安装/卸载/禁用后宿主发布；UI 收到后重新 list_plugins，spec §6.3）。
 *  注：Rust 侧 EV_PLUGINS_RELOADED 常量（T5/T6 命令层）需与本通道逐字一致。 */
export const EV_PLUGINS_RELOADED = 'ab://plugins-reloaded';

/** PluginsReloaded 载荷：发现明细（plugins/invalid/shadowed）；UI 只消费「重扫完成」信号。 */
export interface PluginsReloadedPayload {
  plugins?: unknown[];
  invalid?: unknown[];
  shadowed?: unknown[];
}

export interface ProgressPayload {
  file_id: string;
  /** [0,100]; omitted when not estimable. */
  percent?: number;
  records_so_far: number;
  bytes_read?: number;
}

export interface PluginLogPayload {
  plugin_id: string;
  level: 'debug' | 'info' | 'warn' | 'error';
  /** Single raw stderr line (trailing newline stripped). */
  line: string;
  /** Host capture time, UTC ms. */
  ts_ms: number;
}

export interface PluginHealthPayload {
  plugin_id: string;
  /** One event per state-machine transition. */
  state: PluginState;
  prev_state: PluginState;
  /** e.g. exit_code, timeout reason. */
  detail?: string;
}
