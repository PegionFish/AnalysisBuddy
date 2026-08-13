import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import type { MetricNode } from '../ipc/types';
import { SessionProvider, useSession, type SessionAction, type SessionState } from '../state/session';
import FilePanel from './FilePanel';
import MetricTree from './MetricTree';

function renderTree() {
  return render(
    <SessionProvider>
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

/** Import a csv and wait for the ready state + metric tree refresh. */
async function setupReadyFile(path: string): Promise<void> {
  fireEvent.change(screen.getByTestId('path-input'), { target: { value: path } });
  fireEvent.click(screen.getByRole('button', { name: 'Import Files' }));
  await advance(500);
  await advance(10_000);
}

describe('MetricTree (ipc-ui.md §4.3)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('renders file → plugin → metric rows for ready files', async () => {
    renderTree();
    await setupReadyFile('C:\\data\\tree.csv');

    const boxes = screen.getAllByRole('checkbox');
    expect(boxes.length).toBeGreaterThan(0);
    expect(screen.getAllByText('tree.csv').length).toBeGreaterThan(0);
    expect(screen.getByText('Builtin CSV')).toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: /metric_1/ })).toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: /metric_2/ })).toBeInTheDocument();
    expect(screen.getAllByText(/Aggregation:/).length).toBeGreaterThan(0);
  });

  it('toggles a metric and fires query_series for the current window', async () => {
    const spy = vi.spyOn(ipc, 'query_series');
    renderTree();
    await setupReadyFile('C:\\data\\query.csv');

    const metricBox = screen.getByRole('checkbox', { name: /metric_1/ });
    fireEvent.click(metricBox);
    await advance(1_000);

    expect(spy).toHaveBeenCalledTimes(1);
    const args = spy.mock.calls[0][0];
    expect(args.metrics).toHaveLength(1);
    expect(args.metrics[0]).toMatch(/^mock-.+builtin-csv:metric-1$/);
    expect(args.t0_ms).toBe(0);
    expect(args.t1_ms).toBe(600_000);
    expect(args.max_points_per_series).toBe(4000);
    expect(metricBox).toBeChecked();
  });

  it('parent file checkbox checks all descendants and unchecks on second click', async () => {
    renderTree();
    await setupReadyFile('C:\\data\\parent.csv');

    const fileBox = screen.getByRole('checkbox', { name: /parent\.csv/ });
    const metricBoxes = screen.getAllByRole('checkbox', { name: /metric_\d/ });

    fireEvent.click(fileBox);
    expect(fileBox).toBeChecked();
    for (const box of metricBoxes) expect(box).toBeChecked();

    fireEvent.click(fileBox);
    expect(fileBox).not.toBeChecked();
    for (const box of metricBoxes) expect(box).not.toBeChecked();
  });

  it('plugin checkbox becomes checked when all its child metrics are selected', async () => {
    renderTree();
    await setupReadyFile('C:\\data\\tree.csv');

    const pluginBox = screen.getByRole('checkbox', { name: 'Builtin CSV' });
    const metricBoxes = screen.getAllByRole('checkbox', { name: /metric_\d/ });
    expect(metricBoxes.length).toBeGreaterThan(0);

    for (const box of metricBoxes) fireEvent.click(box);

    expect(pluginBox).toBeChecked();
    expect((pluginBox as HTMLInputElement).indeterminate).toBe(false);
  });

  it('plugin checkbox is indeterminate when only some child metrics are selected', async () => {
    renderTree();
    await setupReadyFile('C:\\data\\tree.csv');

    const pluginBox = screen.getByRole('checkbox', { name: 'Builtin CSV' });
    const metricBoxes = screen.getAllByRole('checkbox', { name: /metric_\d/ });
    expect(metricBoxes.length).toBeGreaterThan(1);

    fireEvent.click(metricBoxes[0]);

    expect((pluginBox as HTMLInputElement).indeterminate).toBe(true);
    expect(pluginBox).not.toBeChecked();
  });

  it('file checkbox is checked on full selection and indeterminate on partial selection', async () => {
    renderTree();
    await setupReadyFile('C:\\data\\tree.csv');

    const fileBox = screen.getByRole('checkbox', { name: /tree\.csv/ });
    const metricBoxes = screen.getAllByRole('checkbox', { name: /metric_\d/ });
    expect(metricBoxes.length).toBeGreaterThan(1);

    fireEvent.click(metricBoxes[0]);
    expect((fileBox as HTMLInputElement).indeterminate).toBe(true);
    expect(fileBox).not.toBeChecked();

    for (const box of metricBoxes.slice(1)) fireEvent.click(box);

    expect(fileBox).toBeChecked();
    expect((fileBox as HTMLInputElement).indeterminate).toBe(false);
  });

  it('greys out and disables checkboxes of disabled files', async () => {
    renderTree();
    await setupReadyFile('C:\\data\\disabled.csv');

    const metricBox = screen.getByRole('checkbox', { name: /metric_1/ });
    fireEvent.click(metricBox);
    await advance(1_000);

    fireEvent.click(screen.getByRole('button', { name: 'Disable' }));
    expect(metricBox).toBeDisabled();
    expect(metricBox).not.toBeChecked();
  });
});

// ── P2-01：即时搜索 / 收藏 / 最近使用 / 语义分组 / 空状态 ────────────────────────

interface ProbeApi {
  state: SessionState | null;
  dispatch: React.Dispatch<SessionAction> | null;
}

/** 状态探针：把 SessionState/dispatch 暴露给用例（session.snapshot.test 同风格）。 */
function StateProbe({ api }: { api: ProbeApi }) {
  const { state, dispatch } = useSession();
  api.state = state;
  api.dispatch = dispatch;
  return null;
}

function renderMetricTreeOnly(api: ProbeApi) {
  return render(
    <SessionProvider>
      <StateProbe api={api} />
      <MetricTree />
    </SessionProvider>,
  );
}

function metric(id: string, name: string, unit?: string, description?: string): MetricNode {
  const [fileId, pluginId, metricId] = id.split(':');
  return {
    level: 'metric',
    id,
    file_id: fileId,
    plugin_id: pluginId,
    metric_id: metricId,
    name,
    unit,
    description,
  };
}

/** 自造 HWiNFO 风格指标树：cpu/gpu/mem/battery 各一 + 无关键词命中的 frame_time。 */
function hwinfoTree(): MetricNode[] {
  return [
    {
      level: 'file',
      id: 'f1',
      file_id: 'f1',
      name: 'hw.csv',
      children: [
        {
          level: 'plugin',
          id: 'f1:p1',
          file_id: 'f1',
          plugin_id: 'p1',
          name: 'HWiNFO',
          children: [
            metric('f1:p1:cpu_temp', 'cpu_temp', '°C', 'CPU temperature'),
            metric('f1:p1:gpu_clock', 'gpu_clock', 'MHz', 'GPU core clock'),
            metric('f1:p1:memory_used', 'memory_used', 'MB', 'Physical memory used'),
            metric('f1:p1:battery_level', 'battery_level', '%', 'Battery charge level'),
            metric('f1:p1:frame_time', 'frame_time', 'ms', 'Frame render time'),
          ],
        },
      ],
    },
  ];
}

function manyMetricsTree(count: number): MetricNode[] {
  return [
    {
      level: 'file',
      id: 'f1',
      file_id: 'f1',
      name: 'big.csv',
      children: [
        {
          level: 'plugin',
          id: 'f1:p1',
          file_id: 'f1',
          plugin_id: 'p1',
          name: 'HWiNFO',
          children: Array.from({ length: count }, (_, i) =>
            metric(`f1:p1:metric_${i + 1}`, `metric_${i + 1}`, 'ms'),
          ),
        },
      ],
    },
  ];
}

/** 通过 dispatch 注入自造指标树（真实计时器，不走导入流水线）。 */
function seedTree(api: ProbeApi, count = 5): void {
  act(() => {
    api.dispatch!({ type: 'metrics/set', tree: count === 5 ? hwinfoTree() : manyMetricsTree(count) });
  });
}

describe('MetricTree P2-01（检索/收藏/最近使用/分组）', () => {
  beforeEach(() => {
    vi.useRealTimers();
  });

  const metricBoxes = () =>
    screen.queryAllByRole('checkbox', {
      name: /cpu_temp|gpu_clock|memory_used|battery_level|frame_time/,
    });

  const starOf = (re: RegExp) =>
    within(screen.getByRole('checkbox', { name: re }).closest('li')!).getByRole('button', { name: 'Favorite' });

  it('即时搜索：按名称/单位/描述过滤（大小写不敏感），空输入恢复全量', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderMetricTreeOnly(api);
    seedTree(api);

    expect(metricBoxes()).toHaveLength(5);
    const search = screen.getByRole('searchbox', { name: 'Search metrics' });

    // 名称匹配
    fireEvent.change(search, { target: { value: 'cpu' } });
    expect(metricBoxes()).toHaveLength(1);
    expect(screen.getByRole('checkbox', { name: /cpu_temp/ })).toBeInTheDocument();
    expect(screen.queryByRole('checkbox', { name: /gpu_clock/ })).not.toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: 'HWiNFO' })).toBeInTheDocument();

    // 单位匹配（大小写不敏感）
    fireEvent.change(search, { target: { value: 'MHz' } });
    expect(metricBoxes()).toHaveLength(1);
    expect(screen.getByRole('checkbox', { name: /gpu_clock/ })).toBeInTheDocument();

    // 描述匹配
    fireEvent.change(search, { target: { value: 'frame render' } });
    expect(metricBoxes()).toHaveLength(1);
    expect(screen.getByRole('checkbox', { name: /frame_time/ })).toBeInTheDocument();

    // 无命中 → 空状态
    fireEvent.change(search, { target: { value: 'ZZZ' } });
    expect(metricBoxes()).toHaveLength(0);
    expect(screen.getByText('No matching metrics')).toBeInTheDocument();

    // 清空 → 全量恢复
    fireEvent.change(search, { target: { value: '' } });
    expect(metricBoxes()).toHaveLength(5);
  });

  it('收藏：星标切换、localStorage 持久化、只看收藏过滤、取消后空状态', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    const first = renderMetricTreeOnly(api);
    seedTree(api);

    expect(starOf(/cpu_temp/)).toHaveAttribute('aria-pressed', 'false');
    expect(starOf(/cpu_temp/)).toHaveTextContent('☆');

    fireEvent.click(starOf(/cpu_temp/));
    expect(starOf(/cpu_temp/)).toHaveAttribute('aria-pressed', 'true');
    expect(starOf(/cpu_temp/)).toHaveTextContent('★');
    expect(JSON.parse(localStorage.getItem('ab.metric.favorites')!)).toEqual(['f1:p1:cpu_temp']);

    // 重挂载后收藏仍在（localStorage 持久化）
    first.unmount();
    const api2: ProbeApi = { state: null, dispatch: null };
    renderMetricTreeOnly(api2);
    seedTree(api2);
    expect(starOf(/cpu_temp/)).toHaveAttribute('aria-pressed', 'true');
    expect(starOf(/cpu_temp/)).toHaveTextContent('★');

    // 只看收藏：仅展示已收藏指标
    fireEvent.click(screen.getByRole('checkbox', { name: /Favorites only/ }));
    expect(metricBoxes()).toHaveLength(1);
    expect(screen.getByRole('checkbox', { name: /cpu_temp/ })).toBeInTheDocument();
    expect(screen.queryByRole('checkbox', { name: /gpu_clock/ })).not.toBeInTheDocument();

    // 取消收藏 → 收藏过滤下无匹配
    fireEvent.click(starOf(/cpu_temp/));
    expect(JSON.parse(localStorage.getItem('ab.metric.favorites')!)).toEqual([]);
    expect(screen.getByText('No matching metrics')).toBeInTheDocument();
  });

  it('收藏/最近 localStorage 损坏容错：空集起步且写入正常', () => {
    localStorage.setItem('ab.metric.favorites', '{broken');
    localStorage.setItem('ab.metric.recent', '42');
    const api: ProbeApi = { state: null, dispatch: null };
    renderMetricTreeOnly(api);
    seedTree(api);

    expect(metricBoxes()).toHaveLength(5);
    expect(screen.queryByRole('region', { name: 'Recent' })).not.toBeInTheDocument();

    // 损坏后星标写入仍正常
    fireEvent.click(starOf(/cpu_temp/));
    expect(JSON.parse(localStorage.getItem('ab.metric.favorites')!)).toEqual(['f1:p1:cpu_temp']);

    // 损坏后最近使用写入仍正常
    fireEvent.click(screen.getByRole('checkbox', { name: /gpu_clock/ }));
    expect(JSON.parse(localStorage.getItem('ab.metric.recent')!)).toEqual(['f1:p1:gpu_clock']);
  });

  it('最近使用：勾选记录、去重置顶、最近项点击可勾选', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderMetricTreeOnly(api);
    seedTree(api);

    fireEvent.click(screen.getByRole('checkbox', { name: /cpu_temp/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /gpu_clock/ }));
    expect(JSON.parse(localStorage.getItem('ab.metric.recent')!)).toEqual(['f1:p1:gpu_clock', 'f1:p1:cpu_temp']);

    // 树顶部「最近使用」分区展示最近项
    const recentRegion = screen.getByRole('region', { name: 'Recent' });
    expect(within(recentRegion).getByText('gpu_clock')).toBeInTheDocument();
    expect(within(recentRegion).getByRole('button', { name: /cpu_temp/ })).toHaveAttribute('aria-pressed', 'true');

    // 点击最近项取消勾选（同时去重置顶）
    fireEvent.click(within(recentRegion).getByRole('button', { name: /cpu_temp/ }));
    expect(JSON.parse(localStorage.getItem('ab.metric.recent')!)).toEqual(['f1:p1:cpu_temp', 'f1:p1:gpu_clock']);
    expect(within(recentRegion).getByRole('button', { name: /cpu_temp/ })).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByRole('checkbox', { name: /cpu_temp/ })).not.toBeChecked();

    // 再次点击重新勾选
    fireEvent.click(within(recentRegion).getByRole('button', { name: /cpu_temp/ }));
    expect(within(recentRegion).getByRole('button', { name: /cpu_temp/ })).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('checkbox', { name: /cpu_temp/ })).toBeChecked();
  });

  it('最近使用：超过 10 条仅保留最新 10 条', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderMetricTreeOnly(api);
    seedTree(api, 12);

    for (let i = 1; i <= 12; i++) {
      fireEvent.click(screen.getByRole('checkbox', { name: new RegExp(`metric_${i}ms`) }));
    }
    const stored = JSON.parse(localStorage.getItem('ab.metric.recent')!);
    expect(stored).toHaveLength(10);
    expect(stored[0]).toBe('f1:p1:metric_12');
    expect(stored[9]).toBe('f1:p1:metric_3');
  });

  it('语义分组：插件内按关键词分组小节，无命中落「其他」', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderMetricTreeOnly(api);
    seedTree(api);

    for (const heading of ['cpu', 'gpu', 'mem', 'battery']) {
      expect(screen.getByText(heading)).toBeInTheDocument();
    }

    const otherGroup = screen.getByText('Other').closest('li')!;
    expect(within(otherGroup).getByRole('checkbox', { name: /frame_time/ })).toBeInTheDocument();
    expect(within(otherGroup).queryByRole('checkbox', { name: /gpu_clock/ })).not.toBeInTheDocument();
  });

  it('只看收藏但无任何收藏 → 空状态提示', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderMetricTreeOnly(api);
    seedTree(api);

    fireEvent.click(screen.getByRole('checkbox', { name: /Favorites only/ }));
    expect(screen.getByText('No matching metrics')).toBeInTheDocument();
    expect(metricBoxes()).toHaveLength(0);
  });
});
