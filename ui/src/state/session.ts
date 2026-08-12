/** ui/src/state/session.ts — global session state: Context + useReducer (ipc-ui.md §4).
 *  No third-party state management. The provider owns all IPC side effects and event subscriptions. */

import React, { useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { ipc } from '../ipc/ipc';
import { EV_PLUGINS_RELOADED, EV_PLUGIN_HEALTH, EV_PLUGIN_LOG, EV_PROGRESS } from '../ipc/events';
import type { PluginHealthPayload, PluginLogPayload, ProgressPayload } from '../ipc/events';
import type {
  IpcError,
  ImportResult,
  KeyValueResult,
  Lang,
  LoadResult,
  MetricNode,
  MissingFileEntry,
  PluginInfo,
  SeriesSlice,
  SessionSnapshot,
  Theme,
  TimeRange,
} from '../ipc/types';
import i18n from '../i18n';
import { reportError } from '../lib/globalErrors';

/** Fixed query budget for the current viewport (ipc-ui.md §5.2: ~3× viewport width). */
export const MAX_POINTS_PER_SERIES = 4000;

/** Cursor → key_values_at debounce (ipc-ui.md §5.3: 200ms trailing). */
export const KEYVALUES_DEBOUNCE_MS = 200;

/** Default chart window: 10 minutes starting at mock epoch (series base range). */
export const INITIAL_VIEW_WINDOW = { t0_ms: 0, t1_ms: 600_000 };

/** 任务 19：零跨度/反序数据域的最小兜底窗口（60s，以数据点居中）。
 *  避免 t0==t1 时 ECharts time 轴退化、query_series 查空。 */
export const MIN_FIT_SPAN_MS = 60_000;

/** 多文件时间域并集（min start, max end）；无有效范围返回 null。
 *  非有限值（DTO 异常/缺失字段）逐项忽略。 */
export function unionTimeRange(
  ranges: Iterable<TimeRange | null | undefined>,
): TimeRange | null {
  let start = Number.POSITIVE_INFINITY;
  let end = Number.NEGATIVE_INFINITY;
  let has = false;
  for (const r of ranges) {
    if (!r || !Number.isFinite(r.start_ms) || !Number.isFinite(r.end_ms)) continue;
    has = true;
    if (r.start_ms < start) start = r.start_ms;
    if (r.end_ms > end) end = r.end_ms;
  }
  return has ? { start_ms: start, end_ms: end } : null;
}

/** 数据时间域 → 视口窗口；缺失回落 fallback，零跨度/反序给最小兜底窗口。 */
export function fitWindowForRange(
  range: TimeRange | null,
  fallback: { t0_ms: number; t1_ms: number } = INITIAL_VIEW_WINDOW,
): { t0_ms: number; t1_ms: number } {
  if (!range) return { t0_ms: fallback.t0_ms, t1_ms: fallback.t1_ms };
  if (range.end_ms <= range.start_ms) {
    const half = MIN_FIT_SPAN_MS / 2;
    return { t0_ms: range.start_ms - half, t1_ms: range.start_ms + half };
  }
  return { t0_ms: range.start_ms, t1_ms: range.end_ms };
}

/** ready 文件携带的数据时间域集合（视口适配输入）。 */
export function readyFileTimeRanges(files: ImportResult[]): TimeRange[] {
  return files
    .filter((f) => f.status === 'ready' && f.time_range)
    .map((f) => f.time_range as TimeRange);
}

/** 从当前 state 组装会话快照（契约 C1.5）：selected_metrics 按 file_id 分组
 *  （复合 id `file_id:plugin_id:metric_id` 原样保留——后端不解析，恢复时原样返回）；
 *  chart_view_state.time_range 取当前视口（后端处理全量/初始情形）；
 *  legend_disabled / y_axis_scale 当前 TimelineChart 无对应状态 → 提交默认值（不阻塞）。
 *  全空（无选择/无游标/视口仍为初始）时返回 null——保持旧版 `{ path }` 调用形状
 *  （后端 C1.2：snapshot None/空字段 → 回落空快照，与无快照的旧会话等价）。 */
export function buildSessionSnapshot(state: SessionState): SessionSnapshot | null {
  const selected_metrics: Record<string, string[]> = {};
  for (const id of state.selectedMetrics) {
    const parts = id.split(':');
    if (parts.length !== 3) continue;
    const fileId = parts[0];
    (selected_metrics[fileId] ??= []).push(id);
  }
  const viewWindow = state.viewWindow;
  const empty =
    state.selectedMetrics.size === 0 &&
    state.cursorMs == null &&
    viewWindow.t0_ms === INITIAL_VIEW_WINDOW.t0_ms &&
    viewWindow.t1_ms === INITIAL_VIEW_WINDOW.t1_ms;
  if (empty) return null;
  return {
    selected_metrics,
    chart_view_state: {
      time_range: { start_ms: viewWindow.t0_ms, end_ms: viewWindow.t1_ms },
      legend_disabled: [],
      y_axis_scale: 'shared',
    },
    cursor_ms: state.cursorMs,
  };
}

export interface SessionState {
  files: ImportResult[];
  progress: Record<string, ProgressPayload>;
  plugins: PluginInfo[];
  metricTree: MetricNode[];
  selectedMetrics: Set<string>;
  /** Files disabled from querying (metrics stay unloaded from query args but data is kept, ipc-ui.md §4.2). */
  disabledFiles: Set<string>;
  viewWindow: { t0_ms: number; t1_ms: number };
  cursorMs: number | null;
  series: SeriesSlice[];
  keyValues: KeyValueResult[];
  /** Whether a key-values query is in flight (drives the per-panel loading placeholder). */
  keyValuesPending: boolean;
  lang: Lang;
  theme: Theme;
  /** Out-of-order protection counters for async command results (ipc-ui.md §5.2/§5.3). */
  seriesSeq: number;
  keyValuesSeq: number;
  /** Session-load missing files for the TopBar badge (ipc-ui.md §4.1). */
  missing: MissingFileEntry[];
  /** Session-load reopen failures (files recorded in the session whose reparse did not reach Ready). */
  reopenFailed: MissingFileEntry[];
}

export type SessionAction =
  | { type: 'files/imported'; results: ImportResult[] }
  | { type: 'files/unloaded'; file_id: string }
  | { type: 'files/status'; file_id: string; status: ImportResult['status']; error?: IpcError }
  | { type: 'files/disabled'; file_id: string; disabled: boolean }
  | { type: 'progress/update'; payload: ProgressPayload }
  | { type: 'plugins/set'; plugins: PluginInfo[] }
  | { type: 'plugins/health'; payload: PluginHealthPayload }
  | { type: 'plugins/install'; plugin: PluginInfo }
  | { type: 'plugins/uninstall'; plugin_id: string }
  | { type: 'plugins/enabled'; plugin_id: string; enabled: boolean }
  | { type: 'plugins/update'; plugin: PluginInfo }
  | { type: 'metrics/set'; tree: MetricNode[] }
  | { type: 'metrics/toggle'; ids: string[]; checked: boolean }
  | { type: 'chart/window'; t0_ms: number; t1_ms: number }
  | { type: 'chart/series'; series: SeriesSlice[]; seq: number }
  | { type: 'cursor/set'; ms: number | null }
  | { type: 'keyvalues/set'; results: KeyValueResult[]; seq: number }
  | { type: 'keyvalues/pending'; pending: boolean }
  | { type: 'keyvalues/merge'; results: KeyValueResult[]; seq: number }
  | { type: 'session/reset' }
  | { type: 'session/missing'; entries: MissingFileEntry[] }
  | { type: 'session/reopen_failed'; entries: MissingFileEntry[] }
  | { type: 'lang/set'; lang: Lang }
  | { type: 'theme/set'; theme: Theme };

export function getInitialTheme(): Theme {
  const stored = localStorage.getItem('ab.theme');
  if (stored === 'light' || stored === 'dark') return stored;
  return window.matchMedia?.('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

export function getInitialLang(): Lang {
  const stored = localStorage.getItem('ab.lang');
  if (stored === 'zh' || stored === 'en') return stored;
  return navigator.language.toLowerCase().startsWith('zh') ? 'zh' : 'en';
}

export function initialSessionState(): SessionState {
  return {
    files: [],
    progress: {},
    plugins: [],
    metricTree: [],
    selectedMetrics: new Set<string>(),
    disabledFiles: new Set<string>(),
    viewWindow: INITIAL_VIEW_WINDOW,
    cursorMs: null,
    series: [],
    keyValues: [],
    keyValuesPending: false,
    lang: getInitialLang(),
    theme: getInitialTheme(),
    seriesSeq: 0,
    keyValuesSeq: 0,
    missing: [],
    reopenFailed: [],
  };
}

/** Placeholder entry for a file replaying through the pipeline after load_session: LoadResult exposes only
 *  file ids, so rows are synthesized keyed by file_id and driven to ready by the replayed progress events.
 *  任务 19：附带 LoadResult.time_ranges 透传的该文件数据时间域（视口适配）。 */
function placeholderLoadedFile(fileId: string, timeRange?: TimeRange): ImportResult {
  return {
    file_id: fileId,
    name: fileId,
    path: fileId,
    size_bytes: 0,
    status: 'parsing',
    matched_plugin: null,
    candidate_plugins: [],
    time_range: timeRange,
  };
}

function mergeImported(files: ImportResult[], results: ImportResult[]): ImportResult[] {
  const next = [...files];
  for (const result of results) {
    const byPath = next.findIndex((f) => f.path === result.path);
    const byId = next.findIndex((f) => f.file_id === result.file_id);
    if (byPath >= 0 && byPath !== byId) next.splice(byPath, 1);
    const idx = next.findIndex((f) => f.file_id === result.file_id);
    if (idx >= 0) next[idx] = result;
    else next.push(result);
  }
  return next;
}

export function sessionReducer(state: SessionState, action: SessionAction): SessionState {
  switch (action.type) {
    case 'files/imported':
      return { ...state, files: mergeImported(state.files, action.results) };
    case 'files/unloaded': {
      const files = state.files.filter((f) => f.file_id !== action.file_id);
      const progress = { ...state.progress };
      delete progress[action.file_id];
      const selectedMetrics = new Set(
        [...state.selectedMetrics].filter((id) => !id.startsWith(`${action.file_id}:`)),
      );
      const disabledFiles = new Set(state.disabledFiles);
      disabledFiles.delete(action.file_id);
      // P1-04：卸载文件后其曲线与关键值立即移除（此前只清选择/禁用集）。
      const series = state.series.filter((s) => s.file_id !== action.file_id);
      const keyValues = state.keyValues.filter((r) => r.file_id !== action.file_id);
      return { ...state, files, progress, selectedMetrics, disabledFiles, series, keyValues };
    }
    case 'files/disabled': {
      const disabledFiles = new Set(state.disabledFiles);
      if (action.disabled) disabledFiles.add(action.file_id);
      else disabledFiles.delete(action.file_id);
      // P1-04：禁用文件的曲线与关键值立即移除（数据保留，仅停止查询展示）。
      const series = action.disabled
        ? state.series.filter((s) => s.file_id !== action.file_id)
        : state.series;
      const keyValues = action.disabled
        ? state.keyValues.filter((r) => r.file_id !== action.file_id)
        : state.keyValues;
      return { ...state, disabledFiles, series, keyValues };
    }
    case 'files/status': {
      const files = state.files.map((f) =>
        f.file_id === action.file_id ? { ...f, status: action.status, error: action.error } : f,
      );
      const progress = { ...state.progress };
      if (action.status === 'ready' || action.status === 'error') delete progress[action.file_id];
      return { ...state, files, progress };
    }
    case 'progress/update':
      return { ...state, progress: { ...state.progress, [action.payload.file_id]: action.payload } };
    case 'plugins/set':
      return { ...state, plugins: action.plugins };
    case 'plugins/health': {
      const { plugin_id, state: newState, detail } = action.payload;
      const plugins = state.plugins.map((p) =>
        p.id === plugin_id
          ? {
              ...p,
              state: newState,
              // A reload/restart cycle returning to ready clears the previous failure digest (§4.6).
              last_error: newState === 'ready' ? null : (detail ?? p.last_error),
            }
          : p,
      );
      return { ...state, plugins };
    }
    case 'plugins/install':
    case 'plugins/update': {
      // Upsert the command-returned PluginInfo; the host's PluginsReloaded refetch converges later.
      const plugins = [...state.plugins.filter((p) => p.id !== action.plugin.id), action.plugin];
      return { ...state, plugins };
    }
    case 'plugins/uninstall':
      return { ...state, plugins: state.plugins.filter((p) => p.id !== action.plugin_id) };
    case 'plugins/enabled': {
      // action.enabled = 目标启用态（false → 进禁用集合）。
      const plugins = state.plugins.map((p) =>
        p.id === action.plugin_id ? { ...p, disabled: !action.enabled } : p,
      );
      return { ...state, plugins };
    }
    case 'metrics/set':
      return { ...state, metricTree: action.tree };
    case 'metrics/toggle': {
      const selectedMetrics = new Set(state.selectedMetrics);
      for (const id of action.ids) {
        if (action.checked) selectedMetrics.add(id);
        else selectedMetrics.delete(id);
      }
      // P1-04：指标全取消 → 曲线清空（晚到响应由 query effect 的 seq 推进失效）。
      const series = selectedMetrics.size === 0 ? [] : state.series;
      return { ...state, selectedMetrics, series };
    }
    case 'chart/window':
      return { ...state, viewWindow: { t0_ms: action.t0_ms, t1_ms: action.t1_ms } };
    case 'chart/series':
      if (action.seq < state.seriesSeq) return state;
      return { ...state, series: action.series, seriesSeq: action.seq };
    case 'cursor/set':
      // P1-04：游标清除 → 关键值清空（晚到响应由 cursor effect 的 seq 推进失效）。
      return { ...state, cursorMs: action.ms, keyValues: action.ms === null ? [] : state.keyValues };
    case 'keyvalues/pending':
      return { ...state, keyValuesPending: action.pending };
    case 'keyvalues/set':
      if (action.seq < state.keyValuesSeq) return state;
      return { ...state, keyValues: action.results, keyValuesSeq: action.seq, keyValuesPending: false };
    case 'keyvalues/merge': {
      // Per-file retry follow-up: apply only while the latest accepted query is still current (§5.3).
      if (action.seq !== state.keyValuesSeq) return state;
      const byId = new Map(state.keyValues.map((r) => [r.file_id, r]));
      for (const r of action.results) byId.set(r.file_id, r);
      return { ...state, keyValues: [...byId.values()] };
    }
    case 'session/reset':
      return initialSessionState();
    case 'session/missing':
      return { ...state, missing: action.entries };
    case 'session/reopen_failed':
      return { ...state, reopenFailed: action.entries };
    case 'lang/set':
      return { ...state, lang: action.lang };
    case 'theme/set':
      return { ...state, theme: action.theme };
    default:
      return state;
  }
}

export interface SessionActions {
  importFiles(paths: string[], overrides?: Record<string, { plugin_id: string }>): Promise<void>;
  unloadFile(fileId: string): Promise<void>;
  toggleMetrics(ids: string[], checked: boolean): void;
  setFileDisabled(fileId: string, disabled: boolean): void;
  /** Per-file re-query of the current cursor position (ipc-ui.md §4.5 retry). */
  retryKeyValues(fileId: string): void;
  reloadPlugin(pluginId: string): Promise<void>;
  /** 模块管理器（spec §6.3, task 7）：安装 ZIP（同 id 不同版本时 overwrite=true 覆盖）。 */
  installPluginZip(path: string, overwrite: boolean): Promise<void>;
  uninstallPlugin(pluginId: string): Promise<void>;
  setPluginEnabled(pluginId: string, enabled: boolean): Promise<void>;
  updatePlugin(pluginId: string): Promise<void>;
  /** 任务 19：视口适配当前 ready 文件数据时间域并集（「重置缩放」语义）。 */
  fitViewToData(): void;
  setLang(lang: Lang): void;
  setTheme(theme: Theme): void;
  newSession(): void;
  saveSession(path?: string): Promise<void>;
  saveSessionAs(): Promise<void>;
  openSession(path: string): Promise<void>;
}

export interface SessionContextValue {
  state: SessionState;
  dispatch: React.Dispatch<SessionAction>;
  actions: SessionActions;
  /** Plugin stderr logs by plugin_id (plugin-log channel, appended live). */
  logs: Record<string, PluginLogPayload[]>;
  /** 保存会话失败的可见反馈（任务 17：此前静默无反馈）；null=无错误。 */
  saveError: string | null;
  dismissSaveError(): void;
}

const SessionContext = React.createContext<SessionContextValue | null>(null);

/** 拒绝值→可读文本：ACL/原生拒绝常以纯字符串到达（与 FilePanel 同策略）。 */
function errorMessageOf(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e && typeof e === 'object') {
    if ('message' in e) return String((e as { message: unknown }).message);
    if ('code' in e) return String((e as { code: unknown }).code);
  }
  return '';
}

function errorCodeOf(e: unknown): string {
  return e && typeof e === 'object' && 'code' in e ? String((e as { code: unknown }).code) : '';
}

export function SessionProvider({ children }: { children: React.ReactNode }) {
  const [state, dispatch] = useReducer(sessionReducer, undefined, initialSessionState);
  const [logs, setLogs] = useState<Record<string, PluginLogPayload[]>>({});
  const [saveError, setSaveError] = useState<string | null>(null);
  const querySeqRef = useRef(0);
  const kvSeqRef = useRef(0);
  const kvCursorRef = useRef<number | null>(null);
  const sessionPathRef = useRef<string | null>(null);

  useEffect(() => {
    document.documentElement.dataset.theme = state.theme;
    localStorage.setItem('ab.theme', state.theme);
  }, [state.theme]);

  useEffect(() => {
    localStorage.setItem('ab.lang', state.lang);
    void i18n.changeLanguage(state.lang);
  }, [state.lang]);

  useEffect(() => {
    const unProgress = ipc.listen<ProgressPayload>(EV_PROGRESS, (payload) => {
      dispatch({ type: 'progress/update', payload });
      if (payload.percent !== undefined && payload.percent >= 100) {
        dispatch({ type: 'files/status', file_id: payload.file_id, status: 'ready' });
      }
    });
    const unLog = ipc.listen<PluginLogPayload>(EV_PLUGIN_LOG, (payload) => {
      setLogs((prev) => {
        const next = { ...prev };
        const buf = [...(next[payload.plugin_id] ?? []), payload];
        next[payload.plugin_id] = buf.slice(-200);
        return next;
      });
    });
    const unHealth = ipc.listen<PluginHealthPayload>(EV_PLUGIN_HEALTH, (payload) => {
      dispatch({ type: 'plugins/health', payload });
    });
    // 模块管理器：宿主重扫发现完成（安装/卸载/禁用/更新后）→ 重新拉取列表（spec §6.3）。
    const unReloaded = ipc.listen(EV_PLUGINS_RELOADED, () => {
      void ipc
        .list_plugins()
        .then((plugins) => dispatch({ type: 'plugins/set', plugins }))
        // 任务 21：禁止静默吞错——留痕到 console + 全局错误横幅/持久日志。
        .catch((e) => reportError(e, 'list_plugins'));
    });
    return () => {
      unProgress();
      unLog();
      unHealth();
      unReloaded();
    };
  }, []);

  /** Re-fetch metric tree when the ready-file collection changes (ipc-ui.md §4.3). */
  useEffect(() => {
    const readyIds = state.files.filter((f) => f.status === 'ready').map((f) => f.file_id);
    if (readyIds.length === 0) {
      dispatch({ type: 'metrics/set', tree: [] });
      return;
    }
    let cancelled = false;
    const t = setTimeout(() => {
      void ipc
        .get_metrics({ file_ids: readyIds })
        .then((tree) => {
          if (!cancelled) dispatch({ type: 'metrics/set', tree });
        })
        // 任务 21：禁止静默吞错——留痕到 console + 全局错误横幅/持久日志。
        .catch((e) => reportError(e, 'get_metrics'));
    }, 100);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [state.files]);

  /** 任务 19（核心修复）：视口自动适配数据时间域。
   *  根因：视口恒为 INITIAL_VIEW_WINDOW（epoch 0~600s），query_series 严格按视口查，
   *  真实时间戳数据（相差数十年）永远查空。
   *  语义：仅在文件集合（ready 集合 + 各自时间域）变化时重新适配；
   *  手动缩放（viewWindow 变化）不触发回弹；全部卸载后回落默认视口。 */
  const fitSignatureRef = useRef<string | null>(null);
  const viewWindowRef = useRef(state.viewWindow);
  viewWindowRef.current = state.viewWindow;
  /** P0-01 目标 3（契约 C1.5）：打开的会话含快照视口时，加载期间（loaded 文件未
   *  全部就绪）压制视口自动适配，快照视口优先；全部就绪后以当前签名作基准释放。 */
  const loadedSessionFitRef = useRef<{ loadedIds: Set<string> } | null>(null);
  useEffect(() => {
    const readyFiles = state.files.filter((f) => f.status === 'ready');
    const signature = readyFiles
      .map((f) =>
        `${f.file_id}:${f.time_range ? `${f.time_range.start_ms}-${f.time_range.end_ms}` : 'na'}`,
      )
      .sort()
      .join('|');
    const pending = loadedSessionFitRef.current;
    if (pending) {
      const readyIds = new Set(readyFiles.map((f) => f.file_id));
      const allLoaded = [...pending.loadedIds].every((id) => readyIds.has(id));
      if (!allLoaded) return; // 会话仍在装载：快照视口优先，不触发自动适配
      loadedSessionFitRef.current = null;
      fitSignatureRef.current = signature; // 全部就绪：以当前状态为基准，不再 fit
      return;
    }
    if (signature === fitSignatureRef.current) return;
    fitSignatureRef.current = signature;
    const win = fitWindowForRange(unionTimeRange(readyFiles.map((f) => f.time_range)));
    const cur = viewWindowRef.current;
    if (win.t0_ms === cur.t0_ms && win.t1_ms === cur.t1_ms) return;
    dispatch({ type: 'chart/window', t0_ms: win.t0_ms, t1_ms: win.t1_ms });
  }, [state.files]);

  /** Query the current viewport window whenever selection or window changes (debounced 150ms, §5.2). */
  useEffect(() => {
    const metrics = [...state.selectedMetrics];
    if (metrics.length === 0) {
      // P1-04：无选中指标不得保留旧曲线——清空并推进 seq 使晚到响应失效。
      dispatch({ type: 'chart/series', series: [], seq: ++querySeqRef.current });
      return;
    }
    const wantedFiles = new Set(metrics.map((id) => id.split(':')[0]));
    const fileIds = state.files
      .filter((f) => f.status === 'ready' && !state.disabledFiles.has(f.file_id) && wantedFiles.has(f.file_id))
      .map((f) => f.file_id);
    if (fileIds.length === 0) {
      // P1-04：无可查询文件（如全部禁用/未就绪）不得留旧数据——同上清空。
      dispatch({ type: 'chart/series', series: [], seq: ++querySeqRef.current });
      return;
    }
    const t = setTimeout(() => {
      const seq = ++querySeqRef.current;
      void ipc
        .query_series({
          file_ids: fileIds,
          metrics,
          t0_ms: state.viewWindow.t0_ms,
          t1_ms: state.viewWindow.t1_ms,
          max_points_per_series: MAX_POINTS_PER_SERIES,
        })
        .then((series) => dispatch({ type: 'chart/series', series, seq }))
        // 任务 21：禁止静默吞错（此前 `.catch(() => undefined)` 把 ACL/参数
        // 拒绝全部吞掉，图表空白无任何线索）。
        .catch((e) => reportError(e, 'query_series'));
    }, 150);
    return () => clearTimeout(t);
  }, [state.selectedMetrics, state.viewWindow, state.files, state.disabledFiles]);

  /** Debounced cursor query (ipc-ui.md §5.3: 200ms trailing; key_values_at never rejects, §1.6). */
  useEffect(() => {
    kvCursorRef.current = state.cursorMs;
    const fileIds = state.files
      .filter((f) => f.status === 'ready' && !state.disabledFiles.has(f.file_id))
      .map((f) => f.file_id);
    if (state.cursorMs === null) {
      // P1-04：游标清除时不得保留旧关键值——清空并推进 seq 使晚到响应失效。
      dispatch({ type: 'keyvalues/set', results: [], seq: ++kvSeqRef.current });
      return;
    }
    if (fileIds.length === 0) {
      // P1-04：无可查询文件时同样清空（上游 cursor/set null 已清，此处兜底）。
      dispatch({ type: 'keyvalues/set', results: [], seq: ++kvSeqRef.current });
      return;
    }
    const cursor = state.cursorMs;
    const t = setTimeout(() => {
      const seq = ++kvSeqRef.current;
      dispatch({ type: 'keyvalues/pending', pending: true });
      void ipc
        .key_values_at({ file_ids: fileIds, timestamp_ms: cursor })
        .then((results) => dispatch({ type: 'keyvalues/set', results, seq }))
        .catch((e) => {
          // 任务 21：留痕后再复位 pending（§1.6 整体永不 reject 的 UI 语义不变）。
          reportError(e, 'key_values_at');
          dispatch({ type: 'keyvalues/pending', pending: false });
        });
    }, KEYVALUES_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [state.cursorMs, state.files, state.disabledFiles]);

  const importFiles = useCallback(
    async (paths: string[], overrides?: Record<string, { plugin_id: string }>) => {
      const results = await ipc.import_files({ paths, overrides });
      dispatch({ type: 'files/imported', results });
    },
    [],
  );

  const unloadFile = useCallback(async (fileId: string) => {
    await ipc.unload_file({ file_id: fileId });
    dispatch({ type: 'files/unloaded', file_id: fileId });
  }, []);

  const toggleMetrics = useCallback((ids: string[], checked: boolean) => {
    dispatch({ type: 'metrics/toggle', ids, checked });
  }, []);

  const setFileDisabled = useCallback((fileId: string, disabled: boolean) => {
    dispatch({ type: 'files/disabled', file_id: fileId, disabled });
  }, []);

  /** Single-file re-query at the current cursor; merges into the latest accepted result set (§4.5 retry). */
  const retryKeyValues = useCallback((fileId: string) => {
    const cursor = kvCursorRef.current;
    if (cursor === null) return;
    const seq = kvSeqRef.current;
    void ipc
      .key_values_at({ file_ids: [fileId], timestamp_ms: cursor })
      .then((results) => dispatch({ type: 'keyvalues/merge', results, seq }))
      .catch((e) => reportError(e, 'key_values_at'));
  }, []);

  /** Rebuild a plugin instance via the auxiliary command; badge flips back to ready via health events (§4.6). */
  const reloadPlugin = useCallback(async (pluginId: string) => {
    const info = await ipc.reload_plugin({ plugin_id: pluginId });
    dispatch({ type: 'plugins/set', plugins: state.plugins.map((p) => (p.id === info.id ? info : p)) });
  }, [state.plugins]);

  /** 模块管理器 actions（spec §6.3）：命令成功后立即以返回值更新列表，
   *  宿主 PluginsReloaded 事件随后触发 list_plugins 全量收敛。 */
  const installPluginZip = useCallback(async (path: string, overwrite: boolean) => {
    const info = await ipc.install_plugin_zip({ path, overwrite });
    dispatch({ type: 'plugins/install', plugin: info });
  }, []);

  const uninstallPlugin = useCallback(async (pluginId: string) => {
    await ipc.uninstall_plugin({ plugin_id: pluginId });
    dispatch({ type: 'plugins/uninstall', plugin_id: pluginId });
  }, []);

  const setPluginEnabled = useCallback(async (pluginId: string, enabled: boolean) => {
    await ipc.set_plugin_enabled({ plugin_id: pluginId, enabled });
    dispatch({ type: 'plugins/enabled', plugin_id: pluginId, enabled });
  }, []);

  const updatePlugin = useCallback(async (pluginId: string) => {
    const info = await ipc.update_plugin({ plugin_id: pluginId });
    dispatch({ type: 'plugins/update', plugin: info });
  }, []);

  /** 任务 19：「重置缩放」新语义——适配当前 ready 文件数据时间域并集，
   *  而非固定 INITIAL_VIEW_WINDOW；无数据域时回落默认视口。 */
  const fitViewToData = useCallback(() => {
    const win = fitWindowForRange(unionTimeRange(readyFileTimeRanges(state.files)));
    const cur = viewWindowRef.current;
    if (win.t0_ms === cur.t0_ms && win.t1_ms === cur.t1_ms) return;
    dispatch({ type: 'chart/window', t0_ms: win.t0_ms, t1_ms: win.t1_ms });
  }, [state.files]);

  const setLang = useCallback((lang: Lang) => {
    dispatch({ type: 'lang/set', lang });
  }, []);

  const setTheme = useCallback((theme: Theme) => {
    // Write the dataset synchronously: TimelineChart reads theme colors via getComputedStyle during its own
    // render/effect (children run before the provider effect), so the DOM must already reflect the new theme.
    document.documentElement.dataset.theme = theme;
    localStorage.setItem('ab.theme', theme);
    dispatch({ type: 'theme/set', theme });
  }, []);

  const newSession = useCallback(() => {
    sessionPathRef.current = null;
    loadedSessionFitRef.current = null;
    // P1-04：跨会话晚到响应不得复活旧数据——先推进查询序号再清空。
    const seq = ++querySeqRef.current;
    const kvSeq = ++kvSeqRef.current;
    dispatch({ type: 'session/reset' });
    dispatch({ type: 'chart/series', series: [], seq });
    dispatch({ type: 'keyvalues/set', results: [], seq: kvSeq });
  }, []);

  /** 保存会话（任务 17 修复）：无已知路径时先弹前端另存为对话框；取消静默，
   *  其余失败进错误横幅（此前无 catch + Rust 对话框挂起 → 静默无任何反馈）。
   *  契约 C1：同时提交完整会话快照（选择/视口/游标）。 */
  const saveSession = useCallback(async (path?: string) => {
    setSaveError(null);
    try {
      let target = path ?? sessionPathRef.current ?? undefined;
      if (!target) {
        const picked = await ipc.pickSavePath();
        if (picked === null) return; // 用户取消：静默
        target = picked;
      }
      const snapshot = buildSessionSnapshot(state);
      const meta = await ipc.save_session({
        path: target,
        ...(snapshot ? { snapshot } : {}),
      });
      sessionPathRef.current = meta.path;
    } catch (e) {
      if (errorCodeOf(e) === 'cancelled') return;
      const message = errorMessageOf(e) || i18n.t('common.error.internal');
      setSaveError(i18n.t('workbench.topbar.save_failed', { message }));
    }
  }, [state]);

  const saveSessionAs = useCallback(async () => {
    setSaveError(null);
    try {
      const picked = await ipc.pickSavePath();
      if (picked === null) return; // 用户取消：静默
      const snapshot = buildSessionSnapshot(state);
      const meta = await ipc.save_session({
        path: picked,
        ...(snapshot ? { snapshot } : {}),
      });
      sessionPathRef.current = meta.path;
    } catch (e) {
      if (errorCodeOf(e) === 'cancelled') return;
      const message = errorMessageOf(e) || i18n.t('common.error.internal');
      setSaveError(i18n.t('workbench.topbar.save_failed', { message }));
    }
  }, [state]);

  const dismissSaveError = useCallback(() => setSaveError(null), []);

  const openSession = useCallback(async (path: string) => {
    const result: LoadResult = await ipc.load_session({ path });
    sessionPathRef.current = result.session.path;
    // 原子替换第 1-2 步：先清空，再置 missing/reopenFailed（跨会话晚到响应失效）。
    const seq = ++querySeqRef.current;
    const kvSeq = ++kvSeqRef.current;
    dispatch({ type: 'session/reset' });
    dispatch({ type: 'chart/series', series: [], seq });
    dispatch({ type: 'keyvalues/set', results: [], seq: kvSeq });
    dispatch({ type: 'session/missing', entries: result.missing });
    dispatch({ type: 'session/reopen_failed', entries: result.reopen_failed ?? [] });
    // 原子替换第 3 步：装载占位文件（LoadResult 只带 file ids，由重放的进度事件驱动就绪）。
    const rangeById = new Map((result.time_ranges ?? []).map((r) => [r.file_id, r]));
    if (result.loaded_file_ids.length > 0) {
      dispatch({
        type: 'files/imported',
        results: result.loaded_file_ids.map((fileId) =>
          placeholderLoadedFile(fileId, rangeById.get(fileId)),
        ),
      });
      void ipc
        .get_metrics({ file_ids: result.loaded_file_ids })
        .then((tree) => {
          dispatch({ type: 'metrics/set', tree });
        })
        // 任务 21：禁止静默吞错。
        .catch((e) => reportError(e, 'get_metrics'));
    }
    // 原子替换第 4 步：快照恢复（复合 id 直接入 Set；视口/游标优先于自动适配）。
    const snap = result.snapshot;
    const compositeIds = snap ? Object.values(snap.selected_metrics).flat() : [];
    if (compositeIds.length > 0) {
      dispatch({ type: 'metrics/toggle', ids: compositeIds, checked: true });
    }
    if (snap?.cursor_ms != null) {
      dispatch({ type: 'cursor/set', ms: snap.cursor_ms });
    }
    const restoredRange = snap?.chart_view_state?.time_range;
    if (restoredRange && Number.isFinite(restoredRange.start_ms) && Number.isFinite(restoredRange.end_ms)) {
      dispatch({ type: 'chart/window', t0_ms: restoredRange.start_ms, t1_ms: restoredRange.end_ms });
      loadedSessionFitRef.current = { loadedIds: new Set(result.loaded_file_ids) };
    } else {
      loadedSessionFitRef.current = null;
    }
  }, []);

  const actions: SessionActions = useMemo(
    () => ({
      importFiles,
      unloadFile,
      toggleMetrics,
      setFileDisabled,
      retryKeyValues,
      reloadPlugin,
      installPluginZip,
      uninstallPlugin,
      setPluginEnabled,
      updatePlugin,
      fitViewToData,
      setLang,
      setTheme,
      newSession,
      saveSession,
      saveSessionAs,
      openSession,
    }),
    [importFiles, unloadFile, toggleMetrics, setFileDisabled, retryKeyValues, reloadPlugin, installPluginZip, uninstallPlugin, setPluginEnabled, updatePlugin, fitViewToData, setLang, setTheme, newSession, saveSession, saveSessionAs, openSession],
  );

  const value = useMemo(
    () => ({ state, dispatch, actions, logs, saveError, dismissSaveError }),
    [state, dispatch, actions, logs, saveError, dismissSaveError],
  );

  return React.createElement(SessionContext.Provider, { value }, children);
}

/** Component-side sole entry point; throws when used outside <SessionProvider>. */
export function useSession(): SessionContextValue {
  const ctx = React.useContext(SessionContext);
  if (!ctx) throw new Error('useSession must be used within <SessionProvider>');
  return ctx;
}
