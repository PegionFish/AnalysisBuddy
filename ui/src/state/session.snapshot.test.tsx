/** ui/src/state/session.snapshot.test.tsx — W1-B 会话快照回归（契约 C1/C3.1）：
 *  1. save_session 提交完整快照（selected_metrics 按 file_id 分组 / 视口 / 游标）；
 *  2. openSession 原子替换 + 快照恢复 + 恢复视口优先于自动适配（fit 压制）；
 *  3. 连续打开两个会话无旧曲线/旧关键值残留；
 *  4. P1-04 派生状态清理（卸载/禁用/全取消/游标清除/新会话）。 */

import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import type { SessionSnapshot } from '../ipc/types';
import FilePanel from '../components/FilePanel';
import MetricTree from '../components/MetricTree';
import TopBar from '../components/TopBar';
import { SessionProvider, useSession, type SessionAction, type SessionState } from './session';

interface ProbeApi {
  state: SessionState | null;
  dispatch: React.Dispatch<SessionAction> | null;
}

/** 状态探针：把 SessionState/dispatch 暴露给断言（cursor-zr-click 同风格）。 */
function StateProbe({ api }: { api: ProbeApi }) {
  const { state, dispatch } = useSession();
  api.state = state;
  api.dispatch = dispatch;
  return null;
}

interface SavedPayload {
  path: string;
  files: { file_id: string; path: string }[];
  snapshot?: SessionSnapshot;
}

function seedSession(payload: SavedPayload): void {
  localStorage.setItem(
    'ab.mock.session',
    JSON.stringify({ ...payload, saved_at_ms: 1, file_count: payload.files.length, selected_metric_count: 0 }),
  );
}

function renderWorkbench(api: ProbeApi) {
  return render(
    <SessionProvider>
      <StateProbe api={api} />
      <TopBar route="/" onNavigate={vi.fn()} />
      <FilePanel />
      <MetricTree />
    </SessionProvider>,
  );
}

async function advance(ms: number): Promise<void> {
  const step = 250;
  for (let t = 0; t < ms; t += step) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(Math.min(step, ms - t));
    });
  }
}

async function importPath(path: string): Promise<void> {
  fireEvent.change(screen.getByTestId('path-input'), { target: { value: path } });
  fireEvent.click(screen.getByRole('button', { name: 'Import Files' }));
  await advance(500);
}

async function openSessionPath(path: string): Promise<void> {
  fireEvent.change(screen.getByRole('textbox', { name: 'Session file path' }), { target: { value: path } });
  fireEvent.click(screen.getByRole('button', { name: 'Open Session' }));
  await advance(500);
}

const PLUGIN = 'builtin-csv';
/** 快照 DTO 中 time_range 的形状（start_ms/end_ms）。 */
const SNAPSHOT_RANGE = { start_ms: 10_000, end_ms: 300_000 };
/** 恢复后的视口（t0_ms/t1_ms）。 */
const RESTORED_WINDOW = { t0_ms: 10_000, t1_ms: 300_000 };
const CURSOR_MS = 200_000;

describe('会话快照（契约 C1）', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('save_session 提交完整快照：selected_metrics 按 file_id 分组、视口、游标', async () => {
    const spy = vi.spyOn(ipc, 'save_session');
    const api: ProbeApi = { state: null, dispatch: null };
    renderWorkbench(api);

    await importPath('C:\\data\\snap.csv');
    await advance(10_500);
    expect(api.state!.files[0].status).toBe('ready');

    fireEvent.click(screen.getByRole('checkbox', { name: /metric_1/ }));
    await advance(500);
    act(() => {
      api.dispatch!({ type: 'cursor/set', ms: 123_456 });
    });
    await advance(300);

    spy.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Save Session' }));
    await advance(500);

    expect(spy).toHaveBeenCalledTimes(1);
    const args = spy.mock.calls[0][0];
    expect(args.path).toBeTruthy();
    const fileId = api.state!.files[0].file_id;
    const snap = args.snapshot!;
    expect(Object.keys(snap.selected_metrics)).toEqual([fileId]);
    // 复合 id 原样保留（后端不解析，恢复时原样返回）。
    expect(snap.selected_metrics[fileId]).toEqual([`${fileId}:${PLUGIN}:metric-1`]);
    expect(snap.chart_view_state).toBeDefined();
    expect(snap.chart_view_state!.time_range).toEqual({
      start_ms: api.state!.viewWindow.t0_ms,
      end_ms: api.state!.viewWindow.t1_ms,
    });
    expect(snap.chart_view_state!.legend_disabled).toEqual([]);
    expect(snap.chart_view_state!.y_axis_scale).toBe('shared');
    expect(snap.cursor_ms).toBe(123_456);
  });

  it('打开会话恢复快照（选择/视口/游标），恢复视口优先于自动适配', async () => {
    seedSession({
      path: 'C:\\sessions\\a.absession',
      files: [{ file_id: 'f1', path: 'C:\\data\\a.csv' }],
      snapshot: {
        selected_metrics: { f1: [`f1:${PLUGIN}:metric-1`, `f1:${PLUGIN}:metric-2`] },
        chart_view_state: { time_range: SNAPSHOT_RANGE, legend_disabled: [], y_axis_scale: 'shared' },
        cursor_ms: CURSOR_MS,
      },
    });
    const api: ProbeApi = { state: null, dispatch: null };
    renderWorkbench(api);

    await openSessionPath('C:\\sessions\\a.absession');

    // 快照恢复：选择 / 视口 / 游标（占位文件尚未就绪时即已生效）。
    expect(api.state!.selectedMetrics.has(`f1:${PLUGIN}:metric-1`)).toBe(true);
    expect(api.state!.viewWindow).toEqual(RESTORED_WINDOW);
    expect(api.state!.cursorMs).toBe(CURSOR_MS);

    // 等待重放解析就绪 + 指标树/查询落地。
    await advance(10_000);
    await advance(500);
    expect(api.state!.files[0].status).toBe('ready');
    expect(screen.getByRole('checkbox', { name: /metric_1/ })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: /metric_2/ })).toBeChecked();
    // 恢复视口优先：自动适配（数据域 0..600s）不得覆盖快照视口。
    expect(api.state!.viewWindow).toEqual(RESTORED_WINDOW);
    expect(api.state!.series.length).toBeGreaterThan(0);
    expect(api.state!.series.every((s) => s.file_id === 'f1')).toBe(true);
  });

  it('打开会话无快照时视口走自动适配（不被恢复逻辑干扰）', async () => {
    seedSession({
      path: 'C:\\sessions\\plain.absession',
      files: [{ file_id: 'f2', path: 'C:\\data\\b.csv' }],
    });
    const api: ProbeApi = { state: null, dispatch: null };
    renderWorkbench(api);

    await openSessionPath('C:\\sessions\\plain.absession');
    expect(api.state!.viewWindow).toEqual({ t0_ms: 0, t1_ms: 600_000 });
    expect(api.state!.selectedMetrics.size).toBe(0);
    expect(api.state!.cursorMs).toBeNull();
  });

  it('连续打开两个会话：原子替换，无旧曲线/旧关键值/旧选择残留', async () => {
    const pathA = 'C:\\sessions\\a.absession';
    const pathB = 'C:\\sessions\\b.absession';
    seedSession({
      path: pathA,
      files: [{ file_id: 'f1', path: 'C:\\data\\a.csv' }],
      snapshot: {
        selected_metrics: { f1: [`f1:${PLUGIN}:metric-1`] },
        chart_view_state: { time_range: SNAPSHOT_RANGE, legend_disabled: [], y_axis_scale: 'shared' },
        cursor_ms: CURSOR_MS,
      },
    });
    const api: ProbeApi = { state: null, dispatch: null };
    renderWorkbench(api);

    // 会话 A：恢复选择/游标 → 就绪 → 曲线与关键值落地。
    await openSessionPath(pathA);
    await advance(10_500);
    expect(api.state!.series.length).toBeGreaterThan(0);
    expect(api.state!.keyValues.length).toBeGreaterThan(0);

    // 会话 B：不同文件、不同视口/游标。
    seedSession({
      path: pathB,
      files: [{ file_id: 'f2', path: 'C:\\data\\b.csv' }],
      snapshot: {
        selected_metrics: { f2: [`f2:${PLUGIN}:metric-1`] },
        chart_view_state: { time_range: { start_ms: 5_000, end_ms: 200_000 }, legend_disabled: [], y_axis_scale: 'shared' },
        cursor_ms: 111_111,
      },
    });
    await openSessionPath(pathB);

    // 原子替换：无 A 残留（文件/选择/曲线/关键值/游标/视口/missing/reopenFailed）。
    expect(api.state!.files.map((f) => f.file_id)).toEqual(['f2']);
    expect(api.state!.selectedMetrics.has(`f1:${PLUGIN}:metric-1`)).toBe(false);
    expect(api.state!.selectedMetrics.has(`f2:${PLUGIN}:metric-1`)).toBe(true);
    // P0-01 后：重开 rows 由响应即时 ready，B 的曲线/关键值可在打开瞬间到达——
    // 断言重点是 A 的数据不残留（若出现 f1 即回归）。
    expect(api.state!.series.every((s) => s.file_id === 'f2')).toBe(true);
    expect(api.state!.keyValues.every((r) => r.file_id === 'f2')).toBe(true);
    expect(api.state!.cursorMs).toBe(111_111);
    expect(api.state!.viewWindow).toEqual({ t0_ms: 5_000, t1_ms: 200_000 });
    expect(api.state!.missing).toEqual([]);
    expect(api.state!.reopenFailed).toEqual([]);

    // B 就绪后只出 B 的曲线，A 的晚到数据不得复活。
    await advance(10_500);
    expect(api.state!.series.length).toBeGreaterThan(0);
    expect(api.state!.series.every((s) => s.file_id === 'f2')).toBe(true);
  });

  it('打开会话重置 missing/reopenFailed 徽章状态（连续打开场景）', async () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderWorkbench(api);
    seedSession({
      path: 'C:\\sessions\\missing.absession',
      files: [{ file_id: 'f1', path: 'gone.csv' }],
      snapshot: { selected_metrics: { f1: [`f1:${PLUGIN}:metric-1`] }, cursor_ms: 1_000 },
    });
    await openSessionPath('C:\\sessions\\missing.absession');
    expect(api.state!.missing).toHaveLength(1);
    expect(screen.getByTestId('missing-badge')).toBeInTheDocument();

    seedSession({
      path: 'C:\\sessions\\ok.absession',
      files: [{ file_id: 'f2', path: 'C:\\data\\ok.csv' }],
    });
    await openSessionPath('C:\\sessions\\ok.absession');
    expect(api.state!.missing).toEqual([]);
    expect(api.state!.reopenFailed).toEqual([]);
    expect(screen.queryByTestId('missing-badge')).not.toBeInTheDocument();
  });

  it('P1-04 派生清理：全取消/禁用/游标清除/卸载/新会话不残留', async () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderWorkbench(api);

    await importPath('C:\\data\\clean.csv');
    await advance(10_500);
    fireEvent.click(screen.getByRole('checkbox', { name: /metric_1/ }));
    await advance(500);
    act(() => {
      api.dispatch!({ type: 'cursor/set', ms: 120_000 });
    });
    await advance(400);
    expect(api.state!.series.length).toBeGreaterThan(0);
    expect(api.state!.keyValues.length).toBeGreaterThan(0);

    // 指标全取消 → 曲线清空。
    fireEvent.click(screen.getByRole('checkbox', { name: /metric_1/ }));
    expect(api.state!.selectedMetrics.size).toBe(0);
    expect(api.state!.series).toEqual([]);

    // 重新勾选 → 曲线恢复。
    fireEvent.click(screen.getByRole('checkbox', { name: /metric_1/ }));
    await advance(500);
    expect(api.state!.series.length).toBeGreaterThan(0);

    // 禁用文件 → 其曲线与关键值立即移除。
    fireEvent.click(within(screen.getByTestId('file-entry')).getByRole('button', { name: 'Disable' }));
    expect(api.state!.disabledFiles.size).toBe(1);
    expect(api.state!.series).toEqual([]);
    expect(api.state!.keyValues).toEqual([]);

    // 游标清除 → 关键值保持为空。
    act(() => {
      api.dispatch!({ type: 'cursor/set', ms: null });
    });
    expect(api.state!.cursorMs).toBeNull();
    expect(api.state!.keyValues).toEqual([]);

    // 卸载 → 文件/选择/曲线全部清空。
    await advance(100);
    fireEvent.click(within(screen.getByTestId('file-entry')).getByRole('button', { name: 'Enable' }));
    fireEvent.click(screen.getByTestId('unload-btn'));
    await advance(300);
    expect(api.state!.files).toEqual([]);
    expect(api.state!.selectedMetrics.size).toBe(0);
    expect(api.state!.series).toEqual([]);
    expect(api.state!.keyValues).toEqual([]);

    // 新会话 → 无任何残留。
    await importPath('C:\\data\\again.csv');
    await advance(10_500);
    fireEvent.click(screen.getByRole('checkbox', { name: /metric_1/ }));
    await advance(500);
    expect(api.state!.series.length).toBeGreaterThan(0);
    fireEvent.click(screen.getByRole('button', { name: 'New Session' }));
    expect(api.state!.files).toEqual([]);
    expect(api.state!.series).toEqual([]);
    expect(api.state!.keyValues).toEqual([]);
    expect(api.state!.cursorMs).toBeNull();
    expect(api.state!.selectedMetrics.size).toBe(0);
    expect(screen.queryByTestId('file-entry')).not.toBeInTheDocument();
  });
});
