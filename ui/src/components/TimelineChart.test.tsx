import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { formatTime } from '../lib/format';
import { ipc } from '../ipc/ipc';
import type { SeriesSlice } from '../ipc/types';
import { SessionProvider } from '../state/session';
import FilePanel from './FilePanel';
import MetricTree from './MetricTree';
import TopBar from './TopBar';
import TimelineChart from './TimelineChart';

interface SeriesOpt {
  name: string;
  yAxisIndex: number;
  color?: string;
  data: unknown[];
  markLine?: { data: Array<{ xAxis: number }> };
}

interface CapturedOption {
  animation: boolean;
  series: SeriesOpt[];
  yAxis: Array<{ id: string; name?: string }>;
  dataZoom: Array<{ type: string; startValue: number; endValue: number }>;
  xAxis: { min: number; max: number };
  legend: { textStyle: { color?: string } };
}

const echartsMock = vi.hoisted(() => {
  const handlers: Record<string, (params: unknown) => void> = {};
  const calls: { option: CapturedOption }[] = [];
  const instance = {
    setOption: (option: unknown) => {
      calls.push({ option: option as CapturedOption });
    },
    on: (type: string, cb: (params: unknown) => void) => {
      handlers[type] = cb;
    },
    dispose: () => undefined,
    resize: () => undefined,
    clear: () => undefined,
    getOption: () => ({}),
  };
  return { handlers, calls, instance, init: vi.fn(() => instance) };
});

vi.mock('echarts', () => ({
  init: echartsMock.init,
  use: vi.fn(),
}));

function renderChart(extra: React.ReactElement | null = null) {
  return render(
    <SessionProvider>
      <FilePanel />
      <MetricTree />
      <TimelineChart />
      {extra}
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

function lastOption(): CapturedOption {
  return echartsMock.calls[echartsMock.calls.length - 1].option;
}

/** Select the first metric of the ready file and wait for the initial query result. */
async function selectFirstMetric(): Promise<string> {
  fireEvent.click(screen.getByRole('checkbox', { name: /metric_1/ }));
  await advance(1_500);
  const calls = (ipc.query_series as unknown as { mock?: { calls: unknown[][] } }).mock?.calls ?? [];
  const args = calls[calls.length - 1]?.[0] as { metrics: string[] } | undefined;
  return args?.metrics[0] ?? '';
}

describe('TimelineChart (ipc-ui.md §4.4/§5)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    echartsMock.calls.length = 0;
    Object.keys(echartsMock.handlers).forEach((k) => delete echartsMock.handlers[k]);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('shows the empty-state guide when no files are imported and never inits ECharts', () => {
    renderChart();
    expect(screen.getByText('Start Analyzing')).toBeInTheDocument();
    expect(screen.getByText(/Import files and select metrics/)).toBeInTheDocument();
    expect(echartsMock.init).not.toHaveBeenCalled();
  });

  it('shows the no-metrics hint when files exist but nothing is selected', async () => {
    renderChart();
    await setupReadyFile('C:\\data\\hint.csv');
    expect(screen.getByTestId('chart-hint')).toBeInTheDocument();
    expect(echartsMock.init).toHaveBeenCalledTimes(1);
  });

  it('renders selected metrics as `${file} / ${metric}` series split onto unit-grouped Y axes', async () => {
    renderChart();
    await setupReadyFile('C:\\data\\units.csv');
    await selectFirstMetric();

    const opt = lastOption();
    expect(opt.animation).toBe(false);
    expect(opt.series.length).toBeGreaterThanOrEqual(1);
    for (const s of opt.series) {
      expect(s.name).toMatch(/^units\.csv \/ metric_\d+$/);
    }
    expect(opt.yAxis.length).toBeGreaterThanOrEqual(1);
    expect(new Set(opt.yAxis.map((y) => y.id)).size).toBe(opt.yAxis.length);
    expect(opt.xAxis.min).toBe(0);
    expect(opt.xAxis.max).toBe(600_000);
  });

  it('debounces rapid dataZoom drags: a single trailing re-query with the final window', async () => {
    const spy = vi.spyOn(ipc, 'query_series');
    renderChart();
    await setupReadyFile('C:\\data\\zoom.csv');
    await selectFirstMetric();
    expect(spy).toHaveBeenCalledTimes(1);

    act(() => echartsMock.handlers.datazoom({ start: 10, end: 40 }));
    await advance(100);
    act(() => echartsMock.handlers.datazoom({ start: 20, end: 60 }));
    await advance(1_000);

    expect(spy).toHaveBeenCalledTimes(2);
    const args = spy.mock.calls[1][0];
    expect(args.t0_ms).toBe(120_000);
    expect(args.t1_ms).toBe(360_000);
    expect(args.max_points_per_series).toBe(4000);
  });

  it('keeps the old series rendered while a zoom re-query is in flight, then swaps in the new window', async () => {
    const resolvers: Array<(s: SeriesSlice[]) => void> = [];
    vi.spyOn(ipc, 'query_series').mockImplementation(
      () => new Promise<SeriesSlice[]>((resolve) => resolvers.push(resolve)),
    );
    renderChart();
    await setupReadyFile('C:\\data\\keep.csv');
    const metricId = await selectFirstMetric();

    const sliceA: SeriesSlice = {
      file_id: metricId.split(':')[0],
      plugin_id: 'builtin-csv',
      metric_id: 'metric-1',
      point_count: 2,
      downsampled: false,
      points: [{ t_ms: 100, v: 1 }, { t_ms: 200, v: 2 }],
    };
    act(() => resolvers[0]([sliceA]));
    await advance(100);
    expect(lastOption().series).toHaveLength(1);
    expect(lastOption().series[0].data).toEqual([[100, 1], [200, 2]]);

    act(() => echartsMock.handlers.datazoom({ start: 10, end: 40 }));
    await advance(400);
    expect(resolvers).toHaveLength(2);

    act(() => echartsMock.handlers.datazoom({ start: 20, end: 50 }));
    await advance(400);
    expect(resolvers).toHaveLength(3);
    expect(lastOption().series[0].data).toEqual([[100, 1], [200, 2]]);
    expect(lastOption().xAxis.min).toBe(96_000);
    expect(lastOption().xAxis.max).toBe(150_000);

    const sliceC: SeriesSlice = {
      ...sliceA,
      point_count: 1,
      downsampled: true,
      points: [{ t_ms: 150_000, v: 5 }],
    };
    act(() => resolvers[2]([sliceC]));
    await advance(100);
    expect(lastOption().series[0].data).toEqual([[150_000, 5]]);

    const sliceB: SeriesSlice = { ...sliceA, point_count: 1, downsampled: false, points: [{ t_ms: 99_000, v: 9 }] };
    act(() => resolvers[1]([sliceB]));
    await advance(100);
    expect(lastOption().series[0].data).toEqual([[150_000, 5]]);

    const queries = vi.mocked(ipc.query_series).mock.calls;
    expect(queries[1][0].max_points_per_series).toBe(4000);
    expect(queries[2][0].max_points_per_series).toBe(4000);
  });

  it('sets cursorMs on chart click and moves the markLine with it', async () => {
    renderChart();
    await setupReadyFile('C:\\data\\cursor.csv');
    await selectFirstMetric();
    expect(screen.queryByTestId('chart-cursor')).not.toBeInTheDocument();

    act(() => echartsMock.handlers.click({ value: [123_456] }));
    expect(screen.getByTestId('chart-cursor')).toHaveTextContent(`Cursor: ${formatTime(123_456)}`);
    expect(lastOption().series[0].markLine?.data[0].xAxis).toBe(123_456);

    act(() => echartsMock.handlers.click({ value: [500_000] }));
    expect(lastOption().series[0].markLine?.data[0].xAxis).toBe(500_000);
  });

  it('refreshes ECharts theme colors from getComputedStyle tokens on theme switch', async () => {
    const getPropertyValue = vi.fn((name: string) =>
      name === '--ab-chart-cursor' ? 'rgb(255, 0, 0)' : 'rgb(10, 20, 30)',
    );
    vi.spyOn(window, 'getComputedStyle').mockReturnValue({
      getPropertyValue,
    } as unknown as CSSStyleDeclaration);

    renderChart(<TopBar route="/" onNavigate={vi.fn()} />);
    await setupReadyFile('C:\\data\\theme.csv');
    await selectFirstMetric();
    const before = echartsMock.calls.length;
    expect(lastOption().legend.textStyle.color).toBe('rgb(10, 20, 30)');

    fireEvent.click(screen.getByRole('button', { name: 'Theme' }));
    await advance(50);

    expect(echartsMock.calls.length).toBeGreaterThan(before);
    expect(lastOption().legend.textStyle.color).toBe('rgb(10, 20, 30)');
    expect(lastOption().series[0].color).toBe('rgb(10, 20, 30)');
    expect(getPropertyValue).toHaveBeenCalledWith('--ab-series-1');
    expect(getPropertyValue).toHaveBeenCalledWith('--ab-chart-cursor');
  });

  it('marks the toolbar as downsampled when any visible series reports downsampled', async () => {
    const resolvers: Array<(s: SeriesSlice[]) => void> = [];
    vi.spyOn(ipc, 'query_series').mockImplementation(
      () => new Promise<SeriesSlice[]>((resolve) => resolvers.push(resolve)),
    );
    renderChart();
    await setupReadyFile('C:\\data\\dsp.csv');
    const metricId = await selectFirstMetric();

    act(() =>
      resolvers[0]([
        {
          file_id: metricId.split(':')[0],
          plugin_id: 'builtin-csv',
          metric_id: 'metric-1',
          point_count: 4000,
          downsampled: true,
          points: [{ t_ms: 100, v: 1 }],
        },
      ]),
    );
    await advance(100);
    expect(screen.getByTestId('chart-downsampled')).toBeInTheDocument();
  });

  it('reset-zoom button restores the initial window', async () => {
    renderChart();
    await setupReadyFile('C:\\data\\reset.csv');
    await selectFirstMetric();

    act(() => echartsMock.handlers.datazoom({ start: 10, end: 40 }));
    await advance(400);
    expect(lastOption().xAxis.min).toBe(60_000);
    expect(lastOption().xAxis.max).toBe(240_000);

    fireEvent.click(screen.getByRole('button', { name: 'Reset Zoom' }));
    await advance(50);
    expect(lastOption().xAxis.min).toBe(0);
    expect(lastOption().xAxis.max).toBe(600_000);
  });
});
