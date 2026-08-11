/** ui/src/components/viewport-fit.test.tsx — 任务 19 回归：时间轴视口自动适配数据时间域。
 *  根因复现背景：视口恒为 INITIAL_VIEW_WINDOW（epoch 0~600s），query_series 严格按视口
 *  查询，真实时间戳 CSV（2026-08-01，≈1.7855e12 ms，与视口相差 56 年）永远查空、画不出曲线。
 *  修复：ImportResultDto.time_range DTO 透传 → SessionProvider 在文件集合变化时
 *  dispatch 视口到数据范围并集。本套件用真实 DTO 形状（含 time_range）灌 real 模式组件树：
 *  1. 单文件导入后视口 = 数据范围（且 query_series 收到数据域参数）；
 *  2. 多文件取并集；3. 移除文件回落默认视口；4. 零跨度兜底；
 *  5. 手动缩放不回弹 +「重置缩放」回到数据范围。 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Dispatch } from 'react';
import { EV_OS_DRAG_DROP } from '../ipc/real';
import type { SessionAction, SessionActions, SessionState } from '../state/session';

/** Mocked Tauri bridge for the real-mode suite. */
const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  open: vi.fn(),
  save: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  // Tauri listen 语义：每次订阅独立事件 id，unlisten 只移除自身订阅。
  // mock 以 last-write-wins 单 handler 模拟：unlisten 仅在仍是当前 handler
  // 时移除（否则恢复前一个），避免旧订阅的 unlisten 误杀新订阅。
  listen: vi.fn(async (channel: string, handler: (event: { payload: unknown }) => void) => {
    const prev = tauri.listeners.get(channel);
    tauri.listeners.set(channel, handler);
    return () => {
      if (tauri.listeners.get(channel) === handler) {
        if (prev) tauri.listeners.set(channel, prev);
        else tauri.listeners.delete(channel);
      }
    };
  }),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: tauri.open, save: tauri.save }));

/* jsdom canvas 2D stub（真实 ECharts 依赖；与 real-import-flow.test.tsx 同款）。 */
function makeCtxStub(canvas: unknown): Record<string, unknown> {
  const gradient = { addColorStop: () => undefined };
  const target: Record<string, unknown> = {
    canvas,
    measureText: (text: string) => ({ width: String(text).length * 7 }),
    createLinearGradient: () => gradient,
    createRadialGradient: () => gradient,
    createPattern: () => gradient,
    getImageData: () => ({ data: new Uint8ClampedArray(4) }),
  };
  return new Proxy(target, {
    get(t, prop) {
      if (prop in t) return t[prop as string];
      if (typeof prop === 'string') return () => undefined;
      return undefined;
    },
    set(t, prop, value) {
      t[prop as string] = value;
      return true;
    },
  });
}

/* -------------------------------------------------------------------------- */
/* 真实时间戳基准（与 ab-protocol serde_tests / builtin-csv fixture 同源）：    */
/* 2026-08-01 附近，≈1.7855e12 ms —— 与 INITIAL_VIEW_WINDOW(epoch 0) 相差 56 年。*/
/* -------------------------------------------------------------------------- */
const T0 = 1_785_600_000_000;
const T1 = 1_785_603_600_000;

const FILE_A = 'file-a-0000-0000-000000000001';
const FILE_B = 'file-b-0000-0000-000000000002';

/** ImportResultDto 真实形状：needs_user_choice/error skip-if-none → 键缺失；
 *  任务 19 新增 time_range 透传（ready 文件携带实际数据时间域）。 */
function realImportResult(fileId: string, path: string, timeRange?: { start_ms: number; end_ms: number }): Record<string, unknown> {
  return {
    file_id: fileId,
    path,
    name: path.split('\\').pop() ?? path,
    size_bytes: 1_048_576,
    status: 'ready',
    matched_plugin: { plugin_id: 'builtin-csv', confidence: 0.95, reason: 'csv header detected' },
    candidate_plugins: [{ plugin_id: 'builtin-csv', confidence: 0.95, reason: 'csv header detected' }],
    ...(timeRange ? { time_range: timeRange } : {}),
  };
}

function metricTreeOf(fileId: string, fileName: string): Record<string, unknown>[] {
  return [
    {
      level: 'file',
      id: fileId,
      file_id: fileId,
      name: fileName,
      children: [
        {
          level: 'plugin',
          id: 'builtin-csv',
          file_id: fileId,
          plugin_id: 'builtin-csv',
          name: 'Built-in CSV',
          children: [
            {
              level: 'metric',
              id: `${fileId}:builtin-csv:fps`,
              file_id: fileId,
              plugin_id: 'builtin-csv',
              metric_id: 'fps',
              name: 'Frame Rate',
              unit: 'fps',
              aggregation: 'avg',
            },
          ],
        },
      ],
    },
  ];
}

function wireInvokes(importResults: unknown[], metricTree: unknown[]): void {
  tauri.invoke.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case 'list_plugins':
        return [];
      case 'import_files':
        return importResults;
      case 'get_metrics':
        return metricTree;
      case 'query_series':
        return [];
      case 'key_values_at':
        return [];
      case 'unload_file':
        return {};
      default:
        throw new Error(`unexpected invoke: ${cmd}`);
    }
  });
}

interface ProbeApi {
  state: SessionState | null;
  dispatch: Dispatch<SessionAction> | null;
  actions: SessionActions | null;
}

describe('task 19: viewport auto-fits the data time domain (real DTO, real mode)', () => {
  beforeEach(() => {
    vi.stubEnv('VITE_AB_IPC', 'real');
    vi.stubEnv('MODE', 'production');
    tauri.listeners.clear();
    tauri.invoke.mockReset();

    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation((function (
      this: HTMLCanvasElement,
    ) {
      return makeCtxStub(this);
    }) as unknown as typeof HTMLCanvasElement.prototype.getContext);
    Object.defineProperty(HTMLCanvasElement.prototype, 'width', {
      configurable: true,
      get: () => 800,
      set: () => undefined,
    });
    Object.defineProperty(HTMLCanvasElement.prototype, 'height', {
      configurable: true,
      get: () => 400,
      set: () => undefined,
    });
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.restoreAllMocks();
    vi.resetModules();
  });

  async function renderWithProbe() {
    vi.resetModules();
    const [tl, sessionMod, { default: AppShell }] = await Promise.all([
      import('@testing-library/react'),
      import('../state/session'),
      import('./AppShell'),
    ]);
    const probe: ProbeApi = { state: null, dispatch: null, actions: null };
    function Probe() {
      const ctx = sessionMod.useSession();
      probe.state = ctx.state;
      probe.dispatch = ctx.dispatch;
      probe.actions = ctx.actions;
      return null;
    }
    const Provider = sessionMod.SessionProvider;
    const view = tl.render(
      <Provider>
        <Probe />
        <AppShell />
      </Provider>,
    );
    return { tl, view, probe };
  }

  async function dropFiles(tl: unknown, paths: string[]): Promise<void> {
    const { act } = tl as { act: (cb: () => void | Promise<void>) => Promise<void> };
    await act(async () => {
      tauri.listeners.get(EV_OS_DRAG_DROP)?.({ payload: { paths, position: { x: 0, y: 0 } } });
    });
  }

  it('真实 DTO 导入后视口 = 数据范围，query_series 按数据域查询（复验阻塞项 c）', async () => {
    wireInvokes([realImportResult(FILE_A, 'C:\\data\\run-1.csv', { start_ms: T0, end_ms: T1 })], metricTreeOf(FILE_A, 'run-1.csv'));
    const { tl, view, probe } = await renderWithProbe();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await dropFiles(tl, ['C:\\data\\run-1.csv']);

      // 视口自动适配到数据域（此前恒为 epoch 0~600s → 相差 56 年查空）。
      await tl.waitFor(() => {
        expect(probe.state?.viewWindow).toEqual({ t0_ms: T0, t1_ms: T1 });
      });

      // 勾选 metric → query_series 收到数据域参数（曲线可画出的直接证据）。
      await tl.waitFor(() => {
        expect(view.container.querySelector('input[type="checkbox"]')).toBeTruthy();
      });
      await tl.act(async () => {
        tl.fireEvent.click(view.container.querySelector('input[type="checkbox"]') as HTMLElement);
      });
      await tl.waitFor(
        () => {
          const call = tauri.invoke.mock.calls.find((c) => c[0] === 'query_series');
          expect(call).toBeTruthy();
          const args = call![1] as { t0_ms: number; t1_ms: number };
          expect(args.t0_ms).toBe(T0);
          expect(args.t1_ms).toBe(T1);
        },
        { timeout: 3000 },
      );
    } finally {
      view.unmount();
    }
  });

  it('多文件取已加载文件 time_range 并集（min t0, max t1）', async () => {
    wireInvokes(
      [
        realImportResult(FILE_A, 'C:\\data\\a.csv', { start_ms: T0, end_ms: T1 }),
        realImportResult(FILE_B, 'C:\\data\\b.csv', { start_ms: T0 - 50_000, end_ms: T1 + 900_000 }),
      ],
      [],
    );
    const { tl, view, probe } = await renderWithProbe();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await dropFiles(tl, ['C:\\data\\a.csv', 'C:\\data\\b.csv']);
      await tl.waitFor(() => {
        expect(probe.state?.viewWindow).toEqual({ t0_ms: T0 - 50_000, t1_ms: T1 + 900_000 });
      });
    } finally {
      view.unmount();
    }
  });

  it('移除唯一文件后视口回落默认 INITIAL_VIEW_WINDOW', async () => {
    wireInvokes([realImportResult(FILE_A, 'C:\\data\\run-1.csv', { start_ms: T0, end_ms: T1 })], []);
    const { tl, view, probe } = await renderWithProbe();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await dropFiles(tl, ['C:\\data\\run-1.csv']);
      await tl.waitFor(() => {
        expect(probe.state?.viewWindow).toEqual({ t0_ms: T0, t1_ms: T1 });
      });

      const unloadBtn = view.container.querySelector('[data-testid="unload-btn"]') as HTMLButtonElement;
      expect(unloadBtn).toBeTruthy();
      await tl.act(async () => {
        tl.fireEvent.click(unloadBtn);
      });
      await tl.waitFor(() => {
        expect(probe.state?.viewWindow).toEqual({ t0_ms: 0, t1_ms: 600_000 });
      });
    } finally {
      view.unmount();
    }
  });

  it('零跨度（t0==t1）给最小兜底窗口', async () => {
    wireInvokes([realImportResult(FILE_A, 'C:\\data\\single-row.csv', { start_ms: T0, end_ms: T0 })], []);
    const { tl, view, probe } = await renderWithProbe();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await dropFiles(tl, ['C:\\data\\single-row.csv']);
      await tl.waitFor(() => {
        expect(probe.state?.viewWindow).toEqual({ t0_ms: T0 - 30_000, t1_ms: T0 + 30_000 });
      });
    } finally {
      view.unmount();
    }
  });

  it('time_range 缺失（旧宿主/异常 DTO）回落默认视口，不崩', async () => {
    wireInvokes([realImportResult(FILE_A, 'C:\\data\\legacy.csv')], []);
    const { tl, view, probe } = await renderWithProbe();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await dropFiles(tl, ['C:\\data\\legacy.csv']);
      await tl.waitFor(() => {
        expect(probe.state?.files.length).toBe(1);
      });
      expect(probe.state?.viewWindow).toEqual({ t0_ms: 0, t1_ms: 600_000 });
    } finally {
      view.unmount();
    }
  });

  it('手动缩放不自动回弹；「重置缩放」回到数据范围而非 epoch 0', async () => {
    wireInvokes([realImportResult(FILE_A, 'C:\\data\\run-1.csv', { start_ms: T0, end_ms: T1 })], []);
    const { tl, view, probe } = await renderWithProbe();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await dropFiles(tl, ['C:\\data\\run-1.csv']);
      await tl.waitFor(() => {
        expect(probe.state?.viewWindow).toEqual({ t0_ms: T0, t1_ms: T1 });
      });

      // 模拟用户缩放（dataZoom 回调最终也是 chart/window dispatch）。
      const zoomed = { t0_ms: T0 + 60_000, t1_ms: T0 + 120_000 };
      await tl.act(async () => {
        probe.dispatch?.({ type: 'chart/window', ...zoomed });
      });
      // 文件集合未变 → 不得自动回弹。
      await new Promise((r) => setTimeout(r, 150));
      expect(probe.state?.viewWindow).toEqual(zoomed);

      // 「重置缩放」→ 适配当前数据范围（修复前回固定 epoch 0 视口）。
      const resetBtn = view.container.querySelector('[data-testid="chart-zoom-reset"]') as HTMLButtonElement;
      expect(resetBtn).toBeTruthy();
      await tl.act(async () => {
        tl.fireEvent.click(resetBtn);
      });
      await tl.waitFor(() => {
        expect(probe.state?.viewWindow).toEqual({ t0_ms: T0, t1_ms: T1 });
      });
    } finally {
      view.unmount();
    }
  });
});
