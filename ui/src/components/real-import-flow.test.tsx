/** ui/src/components/real-import-flow.test.tsx — 任务 17 复现用例：
 *  真实打包版导入链路（files/imported → 首渲文件条目 → get_metrics → TimelineChart
 *  hasFiles 翻转 → 真实 ECharts init/setOption）此前仅在 vitest mock fixture 下跑过，
 *  真实 IPC DTO 形状（core/ab-app ImportResultDto/MetricNodeDto，含 skip-if-none 省略键、
 *  一步返回 status='ready'、常规导入不发 percent:100 终态事件）+ 真实 ECharts 属首跑。
 *  本用例以 Rust 侧源码为准构造真实 DTO JSON，灌入 real 模式完整组件树，不 mock echarts。 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
// 预热 echarts 模块图（文件加载期求值一次，不计入单测 testTimeout）：
// 本文件走真实 ECharts init/setOption，且不 mock echarts；此前在渲染前
// resetModules 导致每个测试重载 echarts 全量 bundle（vite-node 转换 1-2.5s/次），
// 首个重测试逼近 5s 超时。顶层 side-effect import 把成本移出测试体，且模块缓存
// 保证 TimelineChart 的动态 import 命中同一份求值（整文件仅求值一次）。
import 'echarts';
import { EV_OS_DRAG_DROP } from '../ipc/real';
import { EV_PLUGIN_HEALTH, EV_PROGRESS } from '../ipc/events';

/** Mocked Tauri bridge (invoke / event layer / dialog plugin) for the real-mode suite. */
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

/* -------------------------------------------------------------------------- */
/* jsdom canvas 2D stub：真实 ECharts（zrender canvas renderer）需要可用的       */
/* CanvasRenderingContext2D；jsdom 原生 getContext 返回 null。                   */
/* -------------------------------------------------------------------------- */

interface CtxStubRecord {
  calls: string[];
}

const ctxRecord: CtxStubRecord = { calls: [] };

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
      if (typeof prop === 'string') ctxRecord.calls.push(`set:${prop}`);
      t[prop as string] = value;
      return true;
    },
  });
}

/* -------------------------------------------------------------------------- */
/* 真实 Rust DTO（逐字段对照 core/ab-app/src/commands/mod.rs 与 query.rs）：      */
/* - ImportResultDto: needs_user_choice/error skip-if-none → 常规导入时键不存在  */
/* - 真实导入 await 完整管线后一步返回 status='ready'（pipeline_bridge.rs）      */
/* - MetricNodeDto: unit/description/children skip-if-none；metric 级 id 复合    */
/* -------------------------------------------------------------------------- */

const REAL_FILE_ID = 'c4f2a1e0-9b3d-4c5e-8f21-abcdef012345';

/** ImportResultDto 成功单插件导入（builtin-csv 0.95 唯一候选，一步 ready）。
 *  任务 19：ready 文件携带实际数据时间域 time_range（真实时间戳，与
 *  ab-protocol serde_tests 基准同源），供视口自动适配消费。 */
const REAL_IMPORT_RESULT = {
  file_id: REAL_FILE_ID,
  path: 'C:\\data\\run-1.csv',
  name: 'run-1.csv',
  size_bytes: 1_048_576,
  status: 'ready',
  matched_plugin: { plugin_id: 'builtin-csv', confidence: 0.95, reason: 'csv header detected' },
  candidate_plugins: [{ plugin_id: 'builtin-csv', confidence: 0.95, reason: 'csv header detected' }],
  // needs_user_choice: false → serde skip_serializing_if → 键缺失
  // error: None → 键缺失
  time_range: { start_ms: 1_785_600_000_000, end_ms: 1_785_603_600_000 },
};

/** PluginInfoDto（last_error 无 skip → 恒 null；capabilities 恒 false，v1 发现侧不可知）。 */
const REAL_PLUGINS = [
  {
    id: 'builtin-csv',
    display_name: 'Built-in CSV',
    version: '0.1.0',
    state: 'ready',
    loaded_file_ids: [REAL_FILE_ID],
    capabilities: { annotate: false, subscribe: false, binary_sidecar: false },
    last_error: null,
  },
];

/** get_metrics 真实树（build_metric_tree）：file → plugin → metric 三级。 */
const REAL_METRIC_TREE = [
  {
    level: 'file',
    id: REAL_FILE_ID,
    file_id: REAL_FILE_ID,
    name: 'run-1.csv',
    children: [
      {
        level: 'plugin',
        id: 'builtin-csv',
        file_id: REAL_FILE_ID,
        plugin_id: 'builtin-csv',
        name: 'Built-in CSV',
        children: [
          {
            level: 'metric',
            id: `${REAL_FILE_ID}:builtin-csv:fps`,
            file_id: REAL_FILE_ID,
            plugin_id: 'builtin-csv',
            metric_id: 'fps',
            name: 'Frame Rate',
            unit: 'fps',
            description: 'Frames per second',
            aggregation: 'avg',
            // metric 级 children: None → 键缺失
          },
          {
            level: 'metric',
            id: `${REAL_FILE_ID}:builtin-csv:frame_time`,
            file_id: REAL_FILE_ID,
            plugin_id: 'builtin-csv',
            metric_id: 'frame_time',
            name: 'Frame Time',
            // unit/description 为 None → 键缺失（真实 schema 允许）
            aggregation: 'last',
          },
        ],
      },
    ],
  },
];

/** query_series 真实切片（SeriesSliceDto：t_ms/v；点落在数据时间域内）。 */
const REAL_SERIES = [
  {
    file_id: REAL_FILE_ID,
    plugin_id: 'builtin-csv',
    metric_id: 'fps',
    point_count: 3,
    downsampled: false,
    points: [
      { t_ms: 1_785_600_001_000, v: 60 },
      { t_ms: 1_785_600_002_000, v: 59 },
      { t_ms: 1_785_600_003_000, v: 61 },
    ],
  },
];

function wireRealInvokes(): void {
  tauri.invoke.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case 'list_plugins':
        return REAL_PLUGINS;
      case 'import_files':
        return [REAL_IMPORT_RESULT];
      case 'get_metrics':
        return REAL_METRIC_TREE;
      case 'query_series':
        return REAL_SERIES;
      case 'key_values_at':
        return [{ file_id: REAL_FILE_ID, entries: [{ key: 'level', value: 'forest', unit: '' }] }];
      case 'save_session':
        return { path: 'C:\\saved.absession', saved_at_ms: 1, file_count: 1, selected_metric_count: 0 };
      case 'unload_file':
      case 'reload_plugin':
        return {};
      default:
        throw new Error(`unexpected invoke: ${cmd}`);
    }
  });
}

describe('task 17: real packaged-app import flow (real DTO + real ECharts)', () => {
  beforeEach(() => {
    vi.stubEnv('VITE_AB_IPC', 'real');
    vi.stubEnv('MODE', 'production');
    tauri.listeners.clear();
    tauri.invoke.mockReset();
    tauri.open.mockReset();
    tauri.save.mockReset();
    ctxRecord.calls.length = 0;

    // jsdom 无 canvas 2d：为真实 ECharts 提供 stub（仅本用例文件内生效）。
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
  });

  async function renderRealWorkbench() {
    // 不再 resetModules：模块图首次渲染时求值（VITE_AB_IPC=real 已由 beforeEach stub），
    // 后续测试命中模块缓存；mock 状态隔离由 beforeEach 的 listeners.clear()/mockReset() 负责。
    const [tl, { SessionProvider }, { default: AppShell }] = await Promise.all([
      import('@testing-library/react'),
      import('../state/session'),
      import('./AppShell'),
    ]);
    const view = tl.render(
      <SessionProvider>
        <AppShell />
      </SessionProvider>,
    );
    return { tl, view };
  }

  it('import confirmation → entry render → metrics → chart keeps the React tree alive', async () => {
    wireRealInvokes();
    const { tl, view } = await renderRealWorkbench();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));

      // —— 生产导入入口：OS 拖放 → import_files（真实一步返回 ready）——
      // 期间管线还会发进度事件（无 percent:100 终态，常规导入 §2.1 不补发）。
      await tl.act(async () => {
        tauri.listeners.get(EV_PROGRESS)?.({
          payload: { file_id: REAL_FILE_ID, percent: 42, records_so_far: 1200, bytes_read: 6144 },
        });
        tauri.listeners.get(EV_PROGRESS)?.({
          payload: { file_id: REAL_FILE_ID, records_so_far: 4800 },
        });
        tauri.listeners.get(EV_PLUGIN_HEALTH)?.({
          payload: { plugin_id: 'builtin-csv', state: 'ready', prev_state: 'parsing' },
        });
        tauri.listeners.get(EV_OS_DRAG_DROP)?.({
          payload: { paths: ['C:\\data\\run-1.csv'], position: { x: 0, y: 0 } },
        });
      });

      // 首渲文件条目：status ready + 插件匹配信息（真实 DTO 无 needs_user_choice 键）。
      await tl.waitFor(() => {
        const entry = view.container.querySelector('[data-testid="file-entry"]');
        expect(entry).toBeTruthy();
        expect(entry?.textContent).toContain('run-1.csv');
      });

      // —— 崩溃断言：files/imported 后整树不得卸载（生产全黑 = root 内容清空）——
      expect(view.container.querySelector('.app-shell')).toBeTruthy();
      expect(view.container.querySelector('[data-testid="timeline-chart-canvas"]')).toBeTruthy();

      // —— 100ms 后 get_metrics → MetricTree 真实树首渲 ——
      await tl.waitFor(
        () => {
          expect(view.container.textContent).toContain('Frame Rate');
        },
        { timeout: 3000 },
      );
      expect(view.container.querySelector('.app-shell')).toBeTruthy();

      // —— hasFiles 已翻转：真实 ECharts init/setOption 已执行（canvas 2d 被使用）——
      expect(ctxRecord.calls.length).toBeGreaterThan(0);

      // —— 勾选 metric → query_series → 真实数据 setOption ——
      const checkbox = view.container.querySelector('input[type="checkbox"]') as HTMLInputElement;
      expect(checkbox).toBeTruthy();
      await tl.act(async () => {
        tl.fireEvent.click(checkbox);
      });
      await tl.waitFor(
        () => {
          const calls = tauri.invoke.mock.calls.map((c) => c[0]);
          expect(calls).toContain('query_series');
        },
        { timeout: 3000 },
      );
      // 数据渲染后整树仍然存活
      await new Promise((r) => setTimeout(r, 120));
      expect(view.container.querySelector('.app-shell')).toBeTruthy();
      expect(view.container.querySelector('[data-testid="file-entry"]')).toBeTruthy();
      expect(view.container.querySelector('[data-testid="timeline-chart-canvas"]')).toBeTruthy();
    } finally {
      view.unmount();
    }
  });

  it('save_session: 前端对话框发起→显式 path 落盘；拒绝进错误横幅；取消静默（任务 17）', async () => {
    wireRealInvokes();
    tauri.save.mockResolvedValue('C:\\saved.absession');
    const { tl, view } = await renderRealWorkbench();
    try {
      // 点「保存会话」：无已知路径 → pickSavePath → save_session(path)
      const saveBtn = [...view.container.querySelectorAll('button')].find((b) =>
        /Save Session|保存会话/.test(b.textContent ?? ''),
      ) as HTMLButtonElement | undefined;
      expect(saveBtn).toBeTruthy();
      await tl.act(async () => {
        tl.fireEvent.click(saveBtn!);
      });
      await tl.waitFor(() => expect(tauri.save).toHaveBeenCalled());
      await tl.waitFor(() => {
        const calls = tauri.invoke.mock.calls.filter((c) => c[0] === 'save_session');
        expect(calls.length).toBe(1);
        expect(calls[0][1]).toEqual({ path: 'C:\\saved.absession' });
      });
      expect(view.container.querySelector('[data-testid="save-error"]')).toBeFalsy();

      // 保存失败 → 错误横幅（不再静默）
      tauri.save.mockResolvedValue('C:\\bad.absession');
      tauri.invoke.mockImplementation(async (cmd: string) => {
        if (cmd === 'save_session') throw { code: 'session_io', message: 'disk full' };
        if (cmd === 'list_plugins') return REAL_PLUGINS;
        return {};
      });
      const saveAsBtn = [...view.container.querySelectorAll('button')].find((b) =>
        /Save As|另存为/.test(b.textContent ?? ''),
      ) as HTMLButtonElement | undefined;
      expect(saveAsBtn).toBeTruthy();
      await tl.act(async () => {
        tl.fireEvent.click(saveAsBtn!);
      });
      await tl.waitFor(() => {
        const banner = view.container.querySelector('[data-testid="save-error"]');
        expect(banner?.textContent).toContain('disk full');
      });

      // 用户取消对话框 → 静默（无横幅、无 save_session 调用新增）
      const before = tauri.invoke.mock.calls.filter((c) => c[0] === 'save_session').length;
      tauri.save.mockResolvedValue(null);
      await tl.act(async () => {
        tl.fireEvent.click(saveAsBtn!);
      });
      await tl.waitFor(() => expect(tauri.save).toHaveBeenCalledTimes(3));
      await new Promise((r) => setTimeout(r, 60));
      expect(tauri.invoke.mock.calls.filter((c) => c[0] === 'save_session').length).toBe(before);
    } finally {
      view.unmount();
    }
  });
});
