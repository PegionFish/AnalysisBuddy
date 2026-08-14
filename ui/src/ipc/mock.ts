/** ui/src/ipc/mock.ts — mock IPC implementation (ipc-ui.md §3.3).
 *  Local state machine + EventEmitter; identical signatures and payload shapes to the real implementation.
 *  Deterministic: all delays, series and key-values derive from seeded LCGs. */

import { EV_PLUGINS_RELOADED, EV_PLUGIN_HEALTH, EV_PLUGIN_LOG, EV_PROGRESS } from './events';
import type { PluginLogPayload, ProgressPayload } from './events';
import type {
  IpcError,
  ImportResult,
  KeyValueResult,
  LoadResult,
  MetricNode,
  PluginInfo,
  PluginMatch,
  PluginState,
  QuerySeriesArgs,
  SeriesSlice,
  SessionMeta,
  SessionSnapshot,
  UpdateInfo,
  UserPreset,
} from './types';
import type { Ipc } from './ipc';
import { Lcg, genKeyValues, genMetricDefs, genMetricTree, genSeries, hashSeed, toSlice } from './fixtures/gen';
import { FIXTURE_PLUGIN, PLUGIN_INFO, matchPluginWithChoiceInjection } from './fixtures/plugins';

const SESSION_KEY = 'ab.mock.session';
/** 用户预设 localStorage 槽位（值 = Record<id, UserPreset> 的 JSON 序列化）。 */
const PRESETS_KEY = 'ab.mock.presets';
const LOG_LIMIT = 200;
/** fixture-csv 模拟更新流落点版本（spec §4.3 mock 约定：update 返回 2.0.0）。 */
const FIXTURE_UPDATE_VERSION = '2.0.0';

type Listener = (payload: unknown) => void;

class Emitter {
  private listeners = new Map<string, Set<Listener>>();

  on(channel: string, cb: Listener): () => void {
    let set = this.listeners.get(channel);
    if (!set) {
      set = new Set();
      this.listeners.set(channel, set);
    }
    set.add(cb);
    return () => {
      set.delete(cb);
    };
  }

  emit(channel: string, payload: unknown): void {
    const set = this.listeners.get(channel);
    if (!set) return;
    for (const cb of [...set]) cb(payload);
  }
}

function err(code: string, message: string, data?: unknown): IpcError {
  return { code, message, data };
}

interface MockFile {
  result: ImportResult;
  pluginId: string | null;
  timer: ReturnType<typeof setInterval> | null;
}

function slugOf(path: string): string {
  const base = path.split(/[\\/]/).pop() ?? path;
  return base.replace(/[^a-zA-Z0-9_-]+/g, '-').slice(0, 32) || 'file';
}

/** id 合法性（镜像 Rust `valid_preset_id`：`^[a-z0-9][a-z0-9-_]{0,63}$`）。 */
const VALID_PRESET_ID_RE = /^[a-z0-9][a-z0-9-_]{0,63}$/;

/** id 由名称生成（slug 化，镜像 Rust `slugify_id`）：源取 name.zh。
 *  小写、非 [a-z0-9] 转 '-'、连续 '-' 折叠、去首尾 '-'（pending 只在后随
 *  字母/数字时落笔）；空 → "preset"；截断 64。 */
function slugifyId(name: string): string {
  let out = '';
  let pendingDash = false;
  for (const ch of name) {
    const cp = ch.charCodeAt(0);
    const isUpper = cp >= 0x41 && cp <= 0x5a;
    const isAlnum = isUpper || (cp >= 0x30 && cp <= 0x39) || (cp >= 0x61 && cp <= 0x7a);
    if (isAlnum) {
      if (pendingDash && out.length > 0) out += '-';
      pendingDash = false;
      out += isUpper ? String.fromCharCode(cp + 0x20) : ch;
    } else {
      pendingDash = true;
    }
  }
  if (out.length === 0) out = 'preset';
  return out.slice(0, 64);
}

/** All live mock instances, so tests can reset state and timers between cases. */
const liveInstances = new Set<{ reset(): void }>();

/** Test/tooling hook: clear every mock instance's files, logs and pending timers. */
export function resetAllMockIpc(): void {
  for (const inst of liveInstances) inst.reset();
}

export function createMockIpc(): Ipc {
  const emitter = new Emitter();
  const files = new Map<string, MockFile>();
  const plugins: PluginInfo[] = PLUGIN_INFO.map((p) => ({ ...p, loaded_file_ids: [...p.loaded_file_ids] }));
  const logs = new Map<string, PluginLogPayload[]>();
  const timers = new Set<ReturnType<typeof setTimeout>>();
  let seqCounter = 0;

  function later(fn: () => void, ms: number): ReturnType<typeof setTimeout> {
    const id = setTimeout(() => {
      timers.delete(id);
      fn();
    }, ms);
    timers.add(id);
    return id;
  }

  function every(fn: () => void, ms: number): ReturnType<typeof setInterval> {
    const id = setInterval(fn, ms);
    timers.add(id);
    return id;
  }

  function cancelAllTimers(): void {
    for (const t of timers) clearTimeout(t);
    timers.clear();
  }

  liveInstances.add({
    reset() {
      cancelAllTimers();
      files.clear();
      logs.clear();
      plugins.splice(0, plugins.length, ...PLUGIN_INFO.map((p) => ({ ...p, loaded_file_ids: [...p.loaded_file_ids] })));
      seqCounter = 0;
    },
  });

  function pushLog(pluginId: string, level: PluginLogPayload['level'], line: string): void {
    let buf = logs.get(pluginId);
    if (!buf) {
      buf = [];
      logs.set(pluginId, buf);
    }
    buf.push({ plugin_id: pluginId, level, line, ts_ms: Date.now() });
    if (buf.length > LOG_LIMIT) buf.splice(0, buf.length - LOG_LIMIT);
    emitter.emit(EV_PLUGIN_LOG, buf[buf.length - 1]);
  }

  function setPluginState(pluginId: string, state: PluginState, prev: PluginState, detail?: string): void {
    const plugin = plugins.find((p) => p.id === pluginId);
    if (!plugin) return;
    plugin.state = state;
    emitter.emit(EV_PLUGIN_HEALTH, { plugin_id: pluginId, state, prev_state: prev, detail });
  }

  function fileExists(fileId: string): boolean {
    return files.has(fileId);
  }

  function markPluginFile(pluginId: string | null, fileId: string, loaded: boolean): void {
    if (!pluginId) return;
    const plugin = plugins.find((p) => p.id === pluginId);
    if (!plugin) return;
    plugin.loaded_file_ids = plugin.loaded_file_ids.filter((id) => id !== fileId);
    if (loaded) plugin.loaded_file_ids.push(fileId);
  }

  function startParse(fileId: string, pluginId: string): void {
    const rng = new Lcg(hashSeed(`parse:${fileId}`));
    const duration = 3000 + rng.next() * 5000;
    const ticks = Math.max(1, Math.floor(duration / 150));
    const totalRecords = 20_000;
    let tick = 0;
    let timer: ReturnType<typeof setInterval> | null = null;

    const progressTimer = every(() => {
      tick += 1;
      const percent = Math.min(100, Math.round((tick / ticks) * 100));
      const payload: ProgressPayload = {
        file_id: fileId,
        percent,
        records_so_far: Math.round((totalRecords * percent) / 100),
        bytes_read: percent * 512,
      };
      emitter.emit(EV_PROGRESS, payload);
      if (percent === 50) pushLog(pluginId, 'warn', `parse ${fileId}: slow batch at 50% (mock)`);
      if (percent >= 100) {
        clearInterval(progressTimer);
        if (timer) clearTimeout(timer);
        const entry = files.get(fileId);
        if (entry) {
          entry.result.status = 'ready';
          entry.result.error = undefined;
          entry.timer = null;
          markPluginFile(pluginId, fileId, true);
          setPluginState(pluginId, 'ready', 'parsing');
          pushLog(pluginId, 'info', `parse ${fileId}: done, ${totalRecords} records`);
        }
      }
    }, 150);

    timer = later(() => {
      clearInterval(progressTimer);
    }, duration + 200);
    const entry = files.get(fileId);
    if (entry) entry.timer = progressTimer;
  }

  function launchPipeline(fileId: string, pluginId: string): void {
    // Steps are chained (each fires only after the previous) so the emitted sequence always matches
    // the state machine order (ipc-ui.md §3.3): discovered→spawning→initializing→ready→parsing.
    const steps: PluginState[] = ['discovered', 'spawning', 'initializing', 'ready', 'parsing'];
    const rng = new Lcg(hashSeed(`health:${fileId}`));
    let prev = 'ready' as PluginState;
    let step = 0;
    const runNext = () => {
      if (step >= steps.length) return;
      const state = steps[step];
      step += 1;
      later(() => {
        if (!fileExists(fileId)) return;
        setPluginState(pluginId, state, prev);
        prev = state;
        if (step === 1) {
          pushLog(pluginId, 'info', `${pluginId} ${PLUGIN_INFO.find((p) => p.id === pluginId)?.version ?? ''} starting`);
          pushLog(pluginId, 'info', `protocol handshake ok (mock)`);
          if (pluginId === 'demo-tool') {
            pushLog(pluginId, 'error', 'locale config missing, falling back to en-US (mock)');
          }
        }
        if (state === 'parsing') startParse(fileId, pluginId);
        else runNext();
      }, 40 + rng.next() * 60);
    };
    runNext();
  }

  function buildResult(
    path: string,
    fileId: string,
    candidates: PluginMatch[],
    matched: PluginMatch | null,
    status: ImportResult['status'],
    needsChoice: boolean,
    error?: IpcError,
  ): ImportResult {
    return {
      file_id: fileId,
      path,
      name: path.split(/[\\/]/).pop() ?? path,
      size_bytes: 2_621_440,
      status,
      matched_plugin: matched,
      candidate_plugins: candidates,
      needs_user_choice: needsChoice,
      error,
    };
  }

  function delay(): Promise<void> {
    const rng = new Lcg(hashSeed(`cmd:${++seqCounter}`));
    const ms = 40 + rng.next() * 110;
    return new Promise((resolve) => later(resolve, ms));
  }

  /** 读取用户预设槽位：损坏/缺失 → 空对象（读写容错）。逐项做形状守卫：
   *  非对象/缺 id/缺 name/entries 非对象 → 静默丢弃该项（与 Rust 侧损坏
   *  回落空集对称，mock 无 stderr 约定）。 */
  function readPresets(): Record<string, UserPreset> {
    try {
      const raw = localStorage.getItem(PRESETS_KEY);
      if (!raw) return {};
      const parsed: unknown = JSON.parse(raw);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
      const store: Record<string, UserPreset> = {};
      for (const [id, item] of Object.entries(parsed as Record<string, unknown>)) {
        if (!item || typeof item !== 'object' || Array.isArray(item)) continue;
        const candidate = item as Partial<UserPreset>;
        if (typeof candidate.id !== 'string' || candidate.id === '') continue;
        if (
          !candidate.name ||
          typeof candidate.name !== 'object' ||
          typeof candidate.name.zh !== 'string' ||
          typeof candidate.name.en !== 'string'
        ) {
          continue;
        }
        if (!candidate.entries || typeof candidate.entries !== 'object' || Array.isArray(candidate.entries)) continue;
        store[id] = candidate as UserPreset;
      }
      return store;
    } catch {
      return {};
    }
  }

  function writePresets(store: Record<string, UserPreset>): void {
    try {
      localStorage.setItem(PRESETS_KEY, JSON.stringify(store));
    } catch {
      /* localStorage 不可用时静默降级 */
    }
  }

  return {
    async list_plugins() {
      await delay();
      return plugins.map((p) => ({ ...p, loaded_file_ids: [...p.loaded_file_ids] }));
    },

    async import_files(args) {
      if (!args.paths || args.paths.length === 0) throw err('invalid_arg', 'paths must be a non-empty array');
      await delay();
      const results: ImportResult[] = [];
      for (const path of args.paths) {
        const lower = path.toLowerCase();
        const overridden = args.overrides?.[path]?.plugin_id;
        if (lower.includes('fail-load')) {
          results.push(
            buildResult(path, `mock-${slugOf(path)}-${++seqCounter}`, [], null, 'error', false, err('file_load_failed', 'mock injection: file load failed')),
          );
          continue;
        }
        if (lower.includes('no-plugin')) {
          results.push(
            buildResult(path, `mock-${slugOf(path)}-${++seqCounter}`, [], null, 'error', false, err('no_plugin_matched', 'mock injection: no plugin matched')),
          );
          continue;
        }

        const candidates = matchPluginWithChoiceInjection(path);
        const sorted = [...candidates].sort((a, b) => b.confidence - a.confidence);

        const existing = [...files.values()].find(
          (f) => f.result.path === path && (f.result.status === 'matched' || f.result.status === 'error'),
        );
        if (existing) {
          const pluginId = overridden ?? existing.pluginId ?? sorted[0]?.plugin_id;
          existing.result.status = 'parsing';
          existing.result.error = undefined;
          existing.result.needs_user_choice = false;
          if (overridden) {
            existing.result.matched_plugin = { plugin_id: pluginId, confidence: 1, reason: 'manual override' };
          }
          existing.pluginId = pluginId;
          if (pluginId) markPluginFile(pluginId, existing.result.file_id, false);
          if (pluginId) launchPipeline(existing.result.file_id, pluginId);
          results.push(existing.result);
          continue;
        }
        const gap = sorted.length >= 2 ? sorted[0].confidence - sorted[1].confidence : 1;
        const needChoice = gap < 0.1;
        const fileId = `mock-${slugOf(path)}-${++seqCounter}${seqCounter % 2 === 0 ? '-odd' : ''}`;
        if (needChoice && !overridden) {
          const result = buildResult(path, fileId, sorted, null, 'matched', true);
          files.set(fileId, { result, pluginId: null, timer: null });
          results.push(result);
          continue;
        }
        const pluginId = overridden ?? sorted[0].plugin_id;
        const matched: PluginMatch =
          overridden ? { plugin_id: pluginId, confidence: 1, reason: 'manual override' } : sorted[0];
        const result = buildResult(path, fileId, sorted, matched, 'parsing', false);
        files.set(fileId, { result, pluginId, timer: null });
        launchPipeline(fileId, pluginId);
        results.push(result);
      }
      return results;
    },

    async unload_file(args) {
      await delay();
      const entry = files.get(args.file_id);
      if (!entry) return;
      if (entry.timer) clearInterval(entry.timer);
      files.delete(args.file_id);
      markPluginFile(entry.pluginId, args.file_id, false);
    },

    async get_metrics(args) {
      await delay();
      const ids = args.file_ids && args.file_ids.length > 0 ? new Set(args.file_ids) : null;
      const nodes: MetricNode[] = [];
      for (const [fileId, entry] of files) {
        if (entry.result.status !== 'ready') continue;
        if (ids && !ids.has(fileId)) continue;
        const pluginId = entry.pluginId;
        if (!pluginId) continue;
        const plugin = plugins.find((p) => p.id === pluginId);
        nodes.push(genMetricTree(fileId, pluginId, plugin?.display_name ?? pluginId));
      }
      return nodes;
    },

    async query_series(args: QuerySeriesArgs) {
      await delay();
      const slices: SeriesSlice[] = [];
      const fileIds = new Set(args.file_ids);
      for (const metricId of args.metrics) {
        const parts = metricId.split(':');
        if (parts.length !== 3) continue;
        const [fileId, pluginId, metricIdPart] = parts;
        if (!fileIds.has(fileId)) continue;
        const entry = files.get(fileId);
        if (!entry || entry.result.status !== 'ready' || entry.pluginId !== pluginId) continue;
        const def = genMetricDefs(fileId).find((d) => d.metric_id === metricIdPart);
        if (!def) continue;
        const { points, downsampled } = genSeries(fileId, pluginId, def, args.t0_ms, args.t1_ms, args.max_points_per_series);
        slices.push(toSlice(fileId, pluginId, metricIdPart, points, downsampled));
      }
      return slices;
    },

    async key_values_at(args) {
      await delay();
      return args.file_ids.map((fileId) => {
        if (fileId.endsWith('-odd')) {
          return { file_id: fileId, error: err('timeout', 'mock injection: per-file query timeout') } satisfies KeyValueResult;
        }
        const entry = files.get(fileId);
        if (!entry || entry.result.status !== 'ready') {
          return { file_id: fileId, error: err('plugin_busy', 'file not ready') } satisfies KeyValueResult;
        }
        return { file_id: fileId, entries: genKeyValues(fileId) } satisfies KeyValueResult;
      });
    },

    async save_session(args) {
      await delay();
      const path = args.path ?? `mock-session-${++seqCounter}.absession`;
      const payload = {
        path,
        saved_at_ms: Date.now(),
        file_count: files.size,
        selected_metric_count: 0,
        files: [...files.values()].map((f) => ({ file_id: f.result.file_id, path: f.result.path })),
        snapshot: args.snapshot,
      };
      localStorage.setItem(SESSION_KEY, JSON.stringify(payload));
      const meta: SessionMeta = {
        path,
        saved_at_ms: payload.saved_at_ms,
        file_count: payload.file_count,
        selected_metric_count: payload.selected_metric_count,
      };
      return meta;
    },

    async load_session(args) {
      await delay();
      const raw = localStorage.getItem(SESSION_KEY);
      if (!raw) throw err('file_not_found', `session file not found: ${args.path}`);
      let payload: {
        path: string;
        files: { file_id: string; path: string }[];
        snapshot?: SessionSnapshot;
      };
      try {
        payload = JSON.parse(raw) as typeof payload;
      } catch {
        throw err('session_io', 'session file is corrupt');
      }
      if (args.path.includes('missing')) {
        return {
          session: { path: args.path, saved_at_ms: 0, file_count: payload.files.length, selected_metric_count: 0 },
          loaded_file_ids: [],
          missing: payload.files.map((f) => ({ path: f.path, reason: 'not_found' as const })),
          snapshot: payload.snapshot,
        } satisfies LoadResult;
      }
      if (args.path.includes('reopen')) {
        return {
          session: { path: args.path, saved_at_ms: 0, file_count: payload.files.length, selected_metric_count: 0 },
          loaded_file_ids: [],
          missing: [],
          reopen_failed: payload.files.map((f) => ({ path: f.path, reason: 'reopen_failed' as const })),
          snapshot: payload.snapshot,
        } satisfies LoadResult;
      }
      if (payload.path !== args.path) throw err('file_not_found', `session file not found: ${args.path}`);
      // P0-01：与真实后端契约一致——`load_session` 内部已 await 完整重放，
      // 响应直接携带 ready 终态行（files），前端写终态、不依赖进度事件
      // （真实 Tauri 事件在响应前已发出，占位行挂载后收不到）。
      const loadedIds: string[] = [];
      const loadedFiles: ImportResult[] = [];
      for (const stored of payload.files) {
        const candidates = matchPluginWithChoiceInjection(stored.path);
        const pluginId = candidates[0]?.plugin_id ?? null;
        if (!pluginId) continue;
        const result = buildResult(
          stored.path,
          stored.file_id,
          candidates,
          candidates[0] ?? null,
          'ready',
          false,
        );
        result.time_range = { start_ms: 0, end_ms: 600_000 };
        files.set(stored.file_id, { result, pluginId, timer: null });
        loadedIds.push(stored.file_id);
        loadedFiles.push(result);
      }
      return {
        session: { path: payload.path, saved_at_ms: 0, file_count: payload.files.length, selected_metric_count: 0 },
        loaded_file_ids: loadedIds,
        files: loadedFiles,
        missing: [],
        snapshot: payload.snapshot,
      } satisfies LoadResult;
    },

    async cancel_parse(args) {
      if (!args.file_id) throw err('invalid_arg', 'file_id is required');
      await delay();
      const entry = files.get(args.file_id);
      if (!entry || entry.result.status !== 'parsing') return; // 未知/终态 → 幂等 Ok
      if (entry.timer) clearInterval(entry.timer);
      entry.timer = null;
      markPluginFile(entry.pluginId, args.file_id, false);
      // 注意：不得原地修改 UI 已持有的 result 对象（import_files 返回值与
      // state 共享同一引用，原地置 error 会让 UI 的竞态守卫误判终态）——
      // 以新对象替换，模拟后端独立状态机。
      entry.result = { ...entry.result, status: 'error', error: err('cancelled', 'parse cancelled') };
      return;
    },

    async get_plugin_log(args) {
      await delay();
      const buf = logs.get(args.plugin_id) ?? [];
      const limit = args.limit ?? 200;
      return buf.slice(-limit);
    },

    async reload_plugin(args) {
      if (!args.plugin_id) throw err('invalid_arg', 'plugin_id is required');
      await delay();
      const plugin = plugins.find((p) => p.id === args.plugin_id);
      if (!plugin) throw err('internal', `plugin not found: ${args.plugin_id}`);
      const prev = plugin.state;
      setPluginState(plugin.id, 'spawning', prev);
      setPluginState(plugin.id, 'initializing', 'spawning');
      setPluginState(plugin.id, 'ready', 'initializing');
      pushLog(plugin.id, 'info', `${plugin.id} reloaded, instance rebuilt (mock)`);
      return { ...plugin };
    },

    /** 模块管理器 mock（spec §4.1）：install 模拟把 fixture-csv 加入内存清单。
     *  路径约定（与 load_session 的 'missing' 注入同风格）：'bad'→module_install、
     *  'protected'→module_protected、'same'→module_conflict(kind=same_version)、
     *  'conflict' 且未 overwrite→module_conflict(kind=different_version)。 */
    async install_plugin_zip(args) {
      await delay();
      const lower = args.path.toLowerCase();
      if (lower.includes('bad')) {
        throw err('module_install', 'mock injection: invalid zip archive');
      }
      if (lower.includes('protected')) {
        throw err('module_protected', 'mock injection: builtin module is protected');
      }
      if (lower.includes('same') && !args.overwrite) {
        throw err('module_conflict', 'mock injection: same id, same version', {
          plugin_id: FIXTURE_PLUGIN.id,
          version: FIXTURE_PLUGIN.version,
          kind: 'same_version',
        });
      }
      if (lower.includes('conflict') && !args.overwrite) {
        throw err('module_conflict', 'mock injection: same id, different version', {
          plugin_id: FIXTURE_PLUGIN.id,
          version: FIXTURE_PLUGIN.version,
          kind: 'different_version',
        });
      }
      const installed: PluginInfo = { ...FIXTURE_PLUGIN, loaded_file_ids: [] };
      const idx = plugins.findIndex((p) => p.id === installed.id);
      if (idx >= 0) plugins[idx] = installed;
      else plugins.push(installed);
      emitter.emit(EV_PLUGINS_RELOADED, {});
      return { ...installed };
    },

    async uninstall_plugin(args) {
      await delay();
      const plugin = plugins.find((p) => p.id === args.plugin_id);
      if (!plugin) throw err('module_not_found', `plugin not found: ${args.plugin_id}`);
      if (plugin.builtin) throw err('module_protected', 'builtin module cannot be uninstalled');
      plugins.splice(plugins.indexOf(plugin), 1);
      emitter.emit(EV_PLUGINS_RELOADED, {});
    },

    async set_plugin_enabled(args) {
      await delay();
      const plugin = plugins.find((p) => p.id === args.plugin_id);
      if (!plugin) throw err('module_not_found', `plugin not found: ${args.plugin_id}`);
      plugin.disabled = !args.enabled;
      emitter.emit(EV_PLUGINS_RELOADED, {});
    },

    async check_plugin_update(args) {
      await delay();
      const plugin = plugins.find((p) => p.id === args.plugin_id);
      if (!plugin) throw err('module_not_found', `plugin not found: ${args.plugin_id}`);
      if (!plugin.update_url) throw err('update_not_available', 'plugin has no update_url');
      const info: UpdateInfo = {
        plugin_id: plugin.id,
        current_version: plugin.version,
        latest_version: '1.2.0',
        is_newer: true,
        asset_name: 'fixture-csv-v1.2.0.zip',
      };
      return info;
    },

    async update_plugin(args) {
      await delay();
      const plugin = plugins.find((p) => p.id === args.plugin_id);
      if (!plugin) throw err('module_not_found', `plugin not found: ${args.plugin_id}`);
      if (!plugin.update_url) throw err('update_not_available', 'plugin has no update_url');
      plugin.version = FIXTURE_UPDATE_VERSION;
      emitter.emit(EV_PLUGINS_RELOADED, {});
      return { ...plugin };
    },

    async list_user_presets() {
      await delay();
      return Object.values(readPresets()).sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    },

    async save_user_preset(args) {
      await delay();
      const zh = args.name.zh.trim();
      const en = args.name.en.trim();
      if (!zh || !en) {
        throw err('invalid_arg', 'name.zh and name.en must be non-empty after trimming');
      }
      const id = slugifyId(zh);
      const store = readPresets();
      if (Object.prototype.hasOwnProperty.call(store, id)) {
        throw err('preset_conflict', `user preset \`${id}\` already exists`);
      }
      const preset: UserPreset = { id, name: args.name, entries: args.entries };
      store[id] = preset;
      writePresets(store);
      return preset;
    },

    async delete_user_preset(args) {
      await delay();
      const { id } = args;
      if (!VALID_PRESET_ID_RE.test(id)) {
        throw err(
          'invalid_arg',
          `invalid user preset id \`${id}\` (must match ^[a-z0-9][a-z0-9-_]{0,63}$)`,
        );
      }
      const store = readPresets();
      if (Object.prototype.hasOwnProperty.call(store, id)) {
        delete store[id];
        writePresets(store);
      }
      return; // 不存在 → 幂等 Ok
    },

    async pickSavePath() {
      // mock 无原生对话框：直接给出确定路径（与 save_session 的默认名一致）。
      await delay();
      return `mock-session-${seqCounter + 1}.absession`;
    },

    async pickOpenSession() {
      // mock 无原生对话框：返回确定的会话路径（与 pickSavePath 同风格）。
      await delay();
      return `mock-session-${seqCounter + 1}.absession`;
    },

    listen<T>(channel: string, cb: (payload: T) => void) {
      return emitter.on(channel, cb as Listener);
    },
  };
}
