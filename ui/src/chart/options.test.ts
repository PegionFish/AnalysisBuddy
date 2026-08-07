import { describe, expect, it } from 'vitest';
import type { SeriesPoint } from '../ipc/types';
import { buildChartOption, type ChartThemeColors, type ResolvedChartSeries } from './options';

function pts(pairs: [number, number][]): SeriesPoint[] {
  return pairs.map(([t_ms, v]) => ({ t_ms, v }));
}

interface CapturedChartOption {
  animation: boolean;
  tooltip: { trigger: string };
  legend: { type: string; top: number; textStyle: { color?: string } };
  grid: { borderColor?: string };
  xAxis: {
    type: string;
    min: number;
    max: number;
    axisLabel: { color?: string };
    axisPointer: { type: string; snap: boolean; lineStyle: { color?: string } };
    splitLine: { lineStyle: { color?: string } };
  };
  yAxis: Array<{
    type: string;
    id: string;
    name?: string;
    position: string;
    axisLabel: { color?: string };
    splitLine: { lineStyle: { color?: string } };
  }>;
  dataZoom: Array<{ type: string; xAxisIndex: number; startValue: number; endValue: number }>;
  series: Array<{
    type: string;
    large: boolean;
    sampling: string;
    progressive: number;
    symbol: string;
    showSymbol: boolean;
    name: string;
    yAxisIndex: number;
    color?: string;
    data: [number, number][];
    markLine?: { symbol: string; silent: boolean; lineStyle: { color?: string }; data: Array<{ xAxis: number }> };
  }>;
}

const SERIES: ResolvedChartSeries[] = [
  { id: 'a', name: 'f1 / m1', unit: 'ms', points: pts([[0, 1], [1000, 2]]), downsampled: false },
  { id: 'b', name: 'f1 / m2', unit: '%', points: pts([[0, 5], [1000, 6]]), downsampled: false },
  { id: 'c', name: 'f1 / m3', unit: 'ms', points: pts([[0, 9], [1000, 8]]), downsampled: false },
];

const WINDOW = { t0_ms: 0, t1_ms: 600_000 };

const COLORS: ChartThemeColors = {
  grid: 'rgb(1, 2, 3)',
  cursor: 'rgb(4, 5, 6)',
  textPrimary: 'rgb(7, 8, 9)',
  textSecondary: 'rgb(10, 11, 12)',
  border: 'rgb(13, 14, 15)',
  series: ['rgb(16, 17, 18)', 'rgb(19, 20, 21)', 'rgb(22, 23, 24)', 'rgb(25, 26, 27)'],
};

function capture(input: Parameters<typeof buildChartOption>[0]): CapturedChartOption {
  return buildChartOption(input) as unknown as CapturedChartOption;
}

describe('buildChartOption (ipc-ui.md §5.1 fixed config)', () => {
  it('locks the fixed config verbatim: animation off, large + lttb + progressive 400 + no symbols', () => {
    const opt = capture({ series: SERIES, window: WINDOW, cursorMs: null });
    expect(opt.animation).toBe(false);
    for (const s of opt.series) {
      expect(s.type).toBe('line');
      expect(s.large).toBe(true);
      expect(s.sampling).toBe('lttb');
      expect(s.progressive).toBe(400);
      expect(s.symbol).toBe('none');
      expect(s.showSymbol).toBe(false);
    }
  });

  it('configures inside + slider dataZoom bound to the current window', () => {
    const opt = capture({ series: SERIES, window: WINDOW, cursorMs: null });
    expect(opt.dataZoom.map((d) => d.type).sort()).toEqual(['inside', 'slider']);
    for (const dz of opt.dataZoom) {
      expect(dz.xAxisIndex).toBe(0);
      expect(dz.startValue).toBe(0);
      expect(dz.endValue).toBe(600_000);
    }
    expect(opt.xAxis.type).toBe('time');
    expect(opt.xAxis.min).toBe(0);
    expect(opt.xAxis.max).toBe(600_000);
  });

  it('groups Y axes by unit: distinct units split, same unit shares an axis', () => {
    const opt = capture({ series: SERIES, window: WINDOW, cursorMs: null });
    expect(opt.yAxis.map((y) => y.id)).toEqual(['ms', '%']);
    expect(opt.yAxis.map((y) => y.name)).toEqual(['ms', '%']);
    expect(opt.yAxis[0].position).toBe('left');
    expect(opt.yAxis[1].position).toBe('right');
    expect(opt.series.map((s) => s.yAxisIndex)).toEqual([0, 1, 0]);
  });

  it('adds a cursor markLine at cursorMs and omits it when the cursor is null', () => {
    const withCursor = capture({ series: SERIES, window: WINDOW, cursorMs: 123_456 });
    for (const s of withCursor.series) {
      expect(s.markLine?.data[0].xAxis).toBe(123_456);
      expect(s.markLine?.silent).toBe(true);
    }
    const noCursor = capture({ series: SERIES, window: WINDOW, cursorMs: null });
    expect(noCursor.series[0].markLine).toBeUndefined();
  });

  it('maps points to [t_ms, v] pairs and keeps the resolved series names', () => {
    const opt = capture({ series: SERIES, window: WINDOW, cursorMs: null });
    expect(opt.series.map((s) => s.name)).toEqual(['f1 / m1', 'f1 / m2', 'f1 / m3']);
    expect(opt.series[0].data).toEqual([[0, 1], [1000, 2]]);
    expect(opt.series[1].data).toEqual([[0, 5], [1000, 6]]);
  });

  it('applies theme colors (getComputedStyle tokens) to series, axes, grid and legend', () => {
    const opt = capture({ series: SERIES, window: WINDOW, cursorMs: 123_456, colors: COLORS });
    expect(opt.series.map((s) => s.color)).toEqual([
      'rgb(16, 17, 18)',
      'rgb(19, 20, 21)',
      'rgb(22, 23, 24)',
    ]);
    expect(opt.legend.textStyle.color).toBe('rgb(7, 8, 9)');
    expect(opt.grid.borderColor).toBe('rgb(13, 14, 15)');
    expect(opt.xAxis.axisLabel.color).toBe('rgb(10, 11, 12)');
    expect(opt.xAxis.axisPointer.lineStyle.color).toBe('rgb(4, 5, 6)');
    expect(opt.yAxis[0].splitLine.lineStyle.color).toBe('rgb(1, 2, 3)');
  });

  it('keeps render-layer lttb sampling for very large (>50k) series — the host LTTB seam', () => {
    const big = pts(Array.from({ length: 60_000 }, (_, i) => [i * 10, Math.sin(i)]));
    const opt = capture({
      series: [{ id: 'big', name: 'f / huge', unit: undefined, points: big, downsampled: true }],
      window: WINDOW,
      cursorMs: null,
    });
    expect(opt.series[0].data).toHaveLength(60_000);
    expect(opt.series[0].large).toBe(true);
    expect(opt.series[0].sampling).toBe('lttb');
    expect(opt.series[0].progressive).toBe(400);
  });

  it('groups unit-less series under a single unnamed axis', () => {
    const unitless = [
      { id: 'x', name: 'f / x', unit: undefined, points: pts([[0, 1]]), downsampled: false },
      { id: 'y', name: 'f / y', unit: undefined, points: pts([[0, 2]]), downsampled: false },
    ];
    const opt = capture({ series: unitless, window: WINDOW, cursorMs: null });
    expect(opt.yAxis).toHaveLength(1);
    expect(opt.yAxis[0].id).toBe('');
    expect(opt.series.map((s) => s.yAxisIndex)).toEqual([0, 0]);
  });
});
