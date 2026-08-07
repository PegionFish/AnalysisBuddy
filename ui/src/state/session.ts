/** ui/src/state/session.ts — global session state: Context + useReducer (ipc-ui.md §4).
 *  No third-party state management. The provider owns all IPC side effects and event subscriptions. */

import React, { useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { ipc } from '../ipc/ipc';
import { EV_PLUGIN_HEALTH, EV_PLUGIN_LOG, EV_PROGRESS } from '../ipc/events';
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
  Theme,
} from '../ipc/types';
import i18n from '../i18n';

/** Fixed query budget for the current viewport (ipc-ui.md §5.2: ~3× viewport width). */
export const MAX_POINTS_PER_SERIES = 4000;

/** Default chart window: 10 minutes starting at mock epoch (series base range). */
export const INITIAL_VIEW_WINDOW = { t0_ms: 0, t1_ms: 600_000 };

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
  lang: Lang;
  theme: Theme;
  /** Out-of-order protection counters for async command results (ipc-ui.md §5.2/§5.3). */
  seriesSeq: number;
  keyValuesSeq: number;
  /** Session-load missing files for the TopBar badge (ipc-ui.md §4.1). */
  missing: MissingFileEntry[];
}

export type SessionAction =
  | { type: 'files/imported'; results: ImportResult[] }
  | { type: 'files/unloaded'; file_id: string }
  | { type: 'files/status'; file_id: string; status: ImportResult['status']; error?: IpcError }
  | { type: 'files/disabled'; file_id: string; disabled: boolean }
  | { type: 'progress/update'; payload: ProgressPayload }
  | { type: 'plugins/set'; plugins: PluginInfo[] }
  | { type: 'plugins/health'; payload: PluginHealthPayload }
  | { type: 'metrics/set'; tree: MetricNode[] }
  | { type: 'metrics/toggle'; ids: string[]; checked: boolean }
  | { type: 'chart/window'; t0_ms: number; t1_ms: number }
  | { type: 'chart/series'; series: SeriesSlice[]; seq: number }
  | { type: 'cursor/set'; ms: number | null }
  | { type: 'keyvalues/set'; results: KeyValueResult[]; seq: number }
  | { type: 'session/reset' }
  | { type: 'session/missing'; entries: MissingFileEntry[] }
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
    lang: getInitialLang(),
    theme: getInitialTheme(),
    seriesSeq: 0,
    keyValuesSeq: 0,
    missing: [],
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
      return { ...state, files, progress, selectedMetrics, disabledFiles };
    }
    case 'files/disabled': {
      const disabledFiles = new Set(state.disabledFiles);
      if (action.disabled) disabledFiles.add(action.file_id);
      else disabledFiles.delete(action.file_id);
      return { ...state, disabledFiles };
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
        p.id === plugin_id ? { ...p, state: newState, last_error: detail ?? p.last_error } : p,
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
      return { ...state, selectedMetrics };
    }
    case 'chart/window':
      return { ...state, viewWindow: { t0_ms: action.t0_ms, t1_ms: action.t1_ms } };
    case 'chart/series':
      if (action.seq < state.seriesSeq) return state;
      return { ...state, series: action.series, seriesSeq: action.seq };
    case 'cursor/set':
      return { ...state, cursorMs: action.ms };
    case 'keyvalues/set':
      if (action.seq < state.keyValuesSeq) return state;
      return { ...state, keyValues: action.results, keyValuesSeq: action.seq };
    case 'session/reset':
      return initialSessionState();
    case 'session/missing':
      return { ...state, missing: action.entries };
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
}

const SessionContext = React.createContext<SessionContextValue | null>(null);

export function SessionProvider({ children }: { children: React.ReactNode }) {
  const [state, dispatch] = useReducer(sessionReducer, undefined, initialSessionState);
  const [logs, setLogs] = useState<Record<string, PluginLogPayload[]>>({});
  const querySeqRef = useRef(0);
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
    return () => {
      unProgress();
      unLog();
      unHealth();
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
        .catch(() => undefined);
    }, 100);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [state.files]);

  /** Query the current viewport window whenever selection or window changes (debounced 150ms, §5.2). */
  useEffect(() => {
    const metrics = [...state.selectedMetrics];
    if (metrics.length === 0) return;
    const wantedFiles = new Set(metrics.map((id) => id.split(':')[0]));
    const fileIds = state.files
      .filter((f) => f.status === 'ready' && !state.disabledFiles.has(f.file_id) && wantedFiles.has(f.file_id))
      .map((f) => f.file_id);
    if (fileIds.length === 0) return;
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
        .catch(() => undefined);
    }, 150);
    return () => clearTimeout(t);
  }, [state.selectedMetrics, state.viewWindow, state.files, state.disabledFiles]);

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

  const setLang = useCallback((lang: Lang) => {
    dispatch({ type: 'lang/set', lang });
  }, []);

  const setTheme = useCallback((theme: Theme) => {
    dispatch({ type: 'theme/set', theme });
  }, []);

  const newSession = useCallback(() => {
    sessionPathRef.current = null;
    dispatch({ type: 'session/reset' });
  }, []);

  const saveSession = useCallback(async (path?: string) => {
    const meta = await ipc.save_session({ path: path ?? sessionPathRef.current ?? undefined });
    sessionPathRef.current = meta.path;
  }, []);

  const saveSessionAs = useCallback(async () => {
    const meta = await ipc.save_session({});
    sessionPathRef.current = meta.path;
  }, []);

  const openSession = useCallback(async (path: string) => {
    const result: LoadResult = await ipc.load_session({ path });
    sessionPathRef.current = result.session.path;
    dispatch({ type: 'session/missing', entries: result.missing });
    if (result.loaded_file_ids.length > 0) {
      void ipc.get_metrics({ file_ids: result.loaded_file_ids }).then((tree) => {
        dispatch({ type: 'metrics/set', tree });
      });
    }
  }, []);

  const actions: SessionActions = useMemo(
    () => ({
      importFiles,
      unloadFile,
      toggleMetrics,
      setFileDisabled,
      setLang,
      setTheme,
      newSession,
      saveSession,
      saveSessionAs,
      openSession,
    }),
    [importFiles, unloadFile, toggleMetrics, setFileDisabled, setLang, setTheme, newSession, saveSession, saveSessionAs, openSession],
  );

  const value = useMemo(
    () => ({ state, dispatch, actions, logs }),
    [state, dispatch, actions, logs],
  );

  return React.createElement(SessionContext.Provider, { value }, children);
}

/** Component-side sole entry point; throws when used outside <SessionProvider>. */
export function useSession(): SessionContextValue {
  const ctx = React.useContext(SessionContext);
  if (!ctx) throw new Error('useSession must be used within <SessionProvider>');
  return ctx;
}
