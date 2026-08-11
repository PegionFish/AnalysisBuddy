/** ui/src/components/cursor-zr-click.test.tsx — 任务 23 回归：真实 ECharts 下
 *  zrender 层点击设置游标（series 级 chart.on('click') 在 large+symbol:none 下
 *  永不触发 → 修复前点击图表游标完全失效、key_values_at 对用户不可达）。
 *  通过 `chart.getZr().handler.dispatch('click', {offsetX, offsetY})` 模拟真实点击：
 *  1. 网格内点击 → cursor/set 派发，时间戳换算落在数据域，key_values_at 被触发；
 *  2. 网格外点击 → 不派发；3. 空 series 状态点击不崩。 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { EV_OS_DRAG_DROP } from '../ipc/real';
import type { SessionState } from '../state/session';

/** Mocked Tauri bridge for the real-mode suite. */
const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  open: vi.fn(),
  save: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (channel: string, handler: (event: { payload: unknown }) => void) => {
    tauri.listeners.set(channel, handler);
    return () => {
      tauri.listeners.delete(channel);
    };
  }),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: tauri.open, save: tauri.save }));

/* jsdom canvas 2D stub（真实 ECharts 依赖）。 */
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

/* 真实时间戳基准（与任务 19 同源；与 INITIAL_VIEW_WINDOW epoch 0 相差 56 年）。 */
const T0 = 1_785_600_000_000;
const T1 = 1_785_603_600_000;
const FILE_A = 'file-a-0000-0000-000000000001';

/** grid 固定 left/right 56，容器 800x400 → 绘图区 x∈[56,744]。 */
const PLOT_LEFT = 56;
const PLOT_WIDTH = 800 - 56 - 56;

/** 模拟真实点击的完整 zrender 事件序列（mousedown→mouseup→click）：
 *  Handler.click 要求 _downEl===_upEl 且 _downPoint 位移 ≤4px（zrender 防误触）；
 *  DOM 代理层会把 offsetX/offsetY 归一化为 zrX/zrY，两者一并附带。 */
function dispatchClick(
  handler: { dispatch(type: string, event: unknown): void },
  offsetX: number,
  offsetY: number,
): void {
  const packet = () => ({
    offsetX,
    offsetY,
    zrX: offsetX,
    zrY: offsetY,
    clientX: offsetX,
    clientY: offsetY,
    target: null,
    stop: () => undefined,
    preventDefault: () => undefined,
  });
  handler.dispatch('mousedown', packet());
  handler.dispatch('mouseup', packet());
  handler.dispatch('click', packet());
}

function realImportResult(): Record<string, unknown> {
  return {
    file_id: FILE_A,
    path: 'C:\\data\\run-1.csv',
    name: 'run-1.csv',
    size_bytes: 1_048_576,
    status: 'ready',
    matched_plugin: { plugin_id: 'builtin-csv', confidence: 0.95, reason: 'csv header detected' },
    candidate_plugins: [{ plugin_id: 'builtin-csv', confidence: 0.95, reason: 'csv header detected' }],
    time_range: { start_ms: T0, end_ms: T1 },
  };
}

function metricTree(): Record<string, unknown>[] {
  return [
    {
      level: 'file',
      id: FILE_A,
      file_id: FILE_A,
      name: 'run-1.csv',
      children: [
        {
          level: 'plugin',
          id: 'builtin-csv',
          file_id: FILE_A,
          plugin_id: 'builtin-csv',
          name: 'Built-in CSV',
          children: [
            {
              level: 'metric',
              id: `${FILE_A}:builtin-csv:fps`,
              file_id: FILE_A,
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

const REAL_SERIES = [
  {
    file_id: FILE_A,
    plugin_id: 'builtin-csv',
    metric_id: 'fps',
    point_count: 3,
    downsampled: false,
    points: [
      { t_ms: T0 + 60_000, v: 60 },
      { t_ms: T0 + 1_800_000, v: 59 },
      { t_ms: T1 - 60_000, v: 61 },
    ],
  },
];

function wireInvokes(): void {
  tauri.invoke.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case 'list_plugins':
        return [];
      case 'import_files':
        return [realImportResult()];
      case 'get_metrics':
        return metricTree();
      case 'query_series':
        return REAL_SERIES;
      case 'key_values_at':
        return [{ file_id: FILE_A, entries: [{ key: 'fps', value: 59, unit: 'fps' }] }];
      case 'unload_file':
        return {};
      default:
        throw new Error(`unexpected invoke: ${cmd}`);
    }
  });
}

describe('task 23: zrender click sets the cursor under real ECharts (large + symbol:none)', () => {
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
    // ECharts init 读容器 clientWidth/clientHeight（jsdom 恒 0 → 布局退化、containPixel 全 false）。
    Object.defineProperty(HTMLElement.prototype, 'clientWidth', {
      configurable: true,
      get: () => 800,
    });
    Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
      configurable: true,
      get: () => 400,
    });
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.restoreAllMocks();
    vi.resetModules();
  });

  async function renderWithProbe() {
    vi.resetModules();
    const [tl, echarts, sessionMod, { default: AppShell }] = await Promise.all([
      import('@testing-library/react'),
      import('echarts'),
      import('../state/session'),
      import('./AppShell'),
    ]);
    const probe: { state: SessionState | null } = { state: null };
    function Probe() {
      probe.state = sessionMod.useSession().state;
      return null;
    }
    const Provider = sessionMod.SessionProvider;
    const view = tl.render(
      <Provider>
        <Probe />
        <AppShell />
      </Provider>,
    );
    return { tl, echarts, view, probe };
  }

  async function importAndSelectMetric(
    tl: { act: (cb: () => void | Promise<void>) => Promise<void>; waitFor: (cb: () => void, opts?: { timeout?: number }) => Promise<void>; fireEvent: { click: (el: HTMLElement) => void } },
    view: { container: HTMLElement },
  ): Promise<void> {
    await tl.act(async () => {
      tauri.listeners.get(EV_OS_DRAG_DROP)?.({
        payload: { paths: ['C:\\data\\run-1.csv'], position: { x: 0, y: 0 } },
      });
    });
    await tl.waitFor(() => {
      expect(view.container.querySelector('input[type="checkbox"]')).toBeTruthy();
    });
    await tl.act(async () => {
      tl.fireEvent.click(view.container.querySelector('input[type="checkbox"]') as HTMLElement);
    });
    await tl.waitFor(
      () => {
        expect(tauri.invoke.mock.calls.some((c) => c[0] === 'query_series')).toBe(true);
      },
      { timeout: 3000 },
    );
  }

  it('网格内 zr click → cursor/set 派发、换算落在数据域、key_values_at 触发（修复前永不触发）', async () => {
    wireInvokes();
    const { tl, echarts, view, probe } = await renderWithProbe();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await importAndSelectMetric(tl, view);

      const canvasEl = view.container.querySelector('[data-testid="timeline-chart-canvas"]') as HTMLElement;
      expect(canvasEl).toBeTruthy();
      const chart = echarts.getInstanceByDom(canvasEl) as unknown as {
        getZr(): { handler: { dispatch(type: string, event: unknown): void } };
      } | undefined;
      expect(chart).toBeTruthy();

      expect(probe.state?.cursorMs).toBeNull();
      // 绘图区中心：x=56+344=400 → t = T0 + (344/688)*(T1-T0) = T0 + 1_800_000。
      const offsetX = PLOT_LEFT + PLOT_WIDTH / 2;
      await tl.act(async () => {
        dispatchClick(chart!.getZr().handler, offsetX, 200);
      });

      expect(probe.state?.cursorMs).toBe(T0 + 1_800_000);
      expect(probe.state!.cursorMs).toBeGreaterThanOrEqual(T0);
      expect(probe.state!.cursorMs!).toBeLessThanOrEqual(T1);
      // 游标可见反馈（TopBar 下的 chart-cursor 徽标）。
      await tl.waitFor(() => {
        expect(view.container.querySelector('[data-testid="chart-cursor"]')).toBeTruthy();
      });
      // cursor → key_values_at（200ms 防抖）：运行时链路对用户重新可达。
      await tl.waitFor(
        () => {
          expect(tauri.invoke.mock.calls.some((c) => c[0] === 'key_values_at')).toBe(true);
        },
        { timeout: 3000 },
      );
    } finally {
      view.unmount();
    }
  });

  it('网格外 zr click 不设游标（containPixel 守卫）', async () => {
    wireInvokes();
    const { tl, echarts, view, probe } = await renderWithProbe();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await importAndSelectMetric(tl, view);

      const canvasEl = view.container.querySelector('[data-testid="timeline-chart-canvas"]') as HTMLElement;
      const chart = echarts.getInstanceByDom(canvasEl) as unknown as {
        getZr(): { handler: { dispatch(type: string, event: unknown): void } };
      };
      expect(chart).toBeTruthy();

      // 左边缘外（grid left=56）与底部滑块区。
      await tl.act(async () => {
        dispatchClick(chart.getZr().handler, 2, 200);
        dispatchClick(chart.getZr().handler, 400, 395);
      });
      expect(probe.state?.cursorMs).toBeNull();
      expect(view.container.querySelector('[data-testid="chart-cursor"]')).toBeFalsy();
    } finally {
      view.unmount();
    }
  });

  it('空 series（未勾选指标）时 zr click 不崩、React 树存活', async () => {
    wireInvokes();
    const { tl, echarts, view } = await renderWithProbe();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_DROP)?.({
          payload: { paths: ['C:\\data\\run-1.csv'], position: { x: 0, y: 0 } },
        });
      });
      await tl.waitFor(() => {
        expect(view.container.querySelector('[data-testid="timeline-chart-canvas"]')).toBeTruthy();
      });

      const canvasEl = view.container.querySelector('[data-testid="timeline-chart-canvas"]') as HTMLElement;
      const chart = echarts.getInstanceByDom(canvasEl) as unknown as {
        getZr(): { handler: { dispatch(type: string, event: unknown): void } };
      } | undefined;
      if (chart) {
        await tl.act(async () => {
          dispatchClick(chart.getZr().handler, 400, 200);
        });
      }
      // 无 series/无数据均不得升级为整树卸载。
      expect(view.container.querySelector('.app-shell')).toBeTruthy();
    } finally {
      view.unmount();
    }
  });
});
