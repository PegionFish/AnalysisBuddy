/** ui/src/chart/options.ts — the single chart-option entry point (ipc-ui.md §5.1 fixed config).
 *  TimelineChart never builds ECharts options inline; everything goes through buildChartOption. */

import type { EChartsOption } from 'echarts';
import type { MetricNode, SeriesPoint, SeriesSlice, Theme } from '../ipc/types';
import { formatTime } from '../lib/format';

export interface ResolvedChartSeries {
  /** Composite metric id (file_id:plugin_id:metric_id) — stable identity across re-queries. */
  id: string;
  /** Legend label: `${fileName} / ${metricName}` (ipc-ui.md §4.4). */
  name: string;
  /** Metric unit; the Y-axis grouping key ('' when absent). */
  unit: string | undefined;
  points: SeriesPoint[];
  /** Whether the pipeline downsampled this slice (field-driven, never a point_count heuristic, §1.0). */
  downsampled: boolean;
}

export interface ChartThemeColors {
  grid?: string;
  cursor?: string;
  textPrimary?: string;
  textSecondary?: string;
  border?: string;
  /** 8-color series palette, cycles per series index (--ab-series-1..8). */
  series?: (string | undefined)[];
}

export interface ChartOptionInput {
  series: ResolvedChartSeries[];
  window: { t0_ms: number; t1_ms: number };
  cursorMs: number | null;
  colors?: ChartThemeColors;
}

/** §5.1 fixed performance config — applied verbatim to every line series. */
const SERIES_BASE = {
  type: 'line',
  large: true,
  sampling: 'lttb',
  progressive: 400,
  symbol: 'none',
  showSymbol: false,
} as const;

/** P6: within one unit group, series whose max scales differ by more than this
 *  factor (one order of magnitude) are split onto separate Y axes. Keeps small
 *  metrics (fps ~60, frame_ms ~15) readable next to large ones (mem_mb ~1000),
 *  which the builtin-csv plugin reports all as unit-less. */
const MAX_AXIS_SCALE_RATIO = 10;

/** P6: horizontal gap (px) between stacked right-hand Y axes (offset). */
const RIGHT_AXIS_GAP = 64;

/** P6: right margin (px) reserved for the first right-hand Y axis labels. */
const RIGHT_AXIS_MARGIN = 56;

/** 空 series 安全选项（任务 17 崩溃根因修复）：导入确认后、勾选指标前，series/yAxis 为空。
 *  此时若仍下发 dataZoom（xAxisIndex:0），ECharts 6 的 CartesianAxisView 会因无 series
 *  绑定、坐标轴 axisBuilder 未构建而同步抛 `Cannot read properties of undefined (reading
 *  'group')`，effect 内抛错 → React 整树卸载（生产全黑）。空 series 时省略 dataZoom，
 *  仅保留 axes/markLine 骨架；dataZoom 随首个 series 一并下发。 */
function emptySeriesOption(window: { t0_ms: number; t1_ms: number }): EChartsOption {
  return {
    animation: false,
    xAxis: { type: 'time', min: window.t0_ms, max: window.t1_ms },
    yAxis: [{ type: 'value' }],
    series: [],
  };
}

/** Resolve slices to chart series: legend name `${fileName} / ${metricName}`, unit for Y-axis grouping. */
export function resolveChartSeries(
  series: SeriesSlice[],
  files: { file_id: string; name: string }[],
  metricTree: MetricNode[],
  selectedMetrics: Set<string>,
): ResolvedChartSeries[] {
  return series
    .filter((slice) => selectedMetrics.has(`${slice.file_id}:${slice.plugin_id}:${slice.metric_id}`))
    .map((slice) => {
      const fileNode = metricTree.find((f) => f.file_id === slice.file_id);
      const pluginNode = fileNode?.children?.find((p) => p.plugin_id === slice.plugin_id);
      const metric = pluginNode?.children?.find((m) => m.metric_id === slice.metric_id);
      const fileName = files.find((f) => f.file_id === slice.file_id)?.name ?? slice.file_id;
      const metricName = metric?.name ?? slice.metric_id;
      return {
        id: `${slice.file_id}:${slice.plugin_id}:${slice.metric_id}`,
        name: `${fileName} / ${metricName}`,
        unit: metric?.unit,
        points: slice.points,
        downsampled: slice.downsampled,
      };
    });
}

/** Theme tokens for ECharts, re-read from computed CSS variables on theme switch (§7).
 *  `theme` is a re-read trigger: setTheme writes the dataset synchronously, so by render time the
 *  computed styles already reflect the new theme. */
export function readChartColors(theme: Theme): ChartThemeColors {
  void theme;
  const cs = window.getComputedStyle(document.documentElement);
  const val = (name: string): string | undefined => {
    const v = cs.getPropertyValue(name).trim();
    return v === '' ? undefined : v;
  };
  return {
    grid: val('--ab-chart-grid'),
    cursor: val('--ab-chart-cursor'),
    textPrimary: val('--ab-text-primary'),
    textSecondary: val('--ab-text-secondary'),
    border: val('--ab-border'),
    series: [1, 2, 3, 4, 5, 6, 7, 8].map((i) => val(`--ab-series-${i}`)),
  };
}

/** One resolved Y axis: stable id (unique across magnitude splits) + display unit label. */
interface ResolvedAxis {
  key: string;
  label: string;
}

/** Max |value| across a series — the scale used for magnitude grouping. */
function seriesMaxAbs(points: SeriesPoint[]): number {
  let max = 0;
  for (const p of points) {
    const a = Math.abs(p.v);
    if (a > max) max = a;
  }
  return max;
}

/** P6: group series into Y axes — first by unit, then (within a unit) split by
 *  value magnitude when scales differ by > MAX_AXIS_SCALE_RATIO. Returns per-series
 *  yAxisIndex plus the resolved axis list (first-encounter order → axis 0 = left).
 *  A unit with a single magnitude bucket keeps the bare unit as its id, so the
 *  common ['ms','%'] case is unchanged; split units get `${unit}#${n}` ids. */
function resolveAxes(series: ResolvedChartSeries[]): { axes: ResolvedAxis[]; yAxisIndex: number[] } {
  const axes: ResolvedAxis[] = [];
  const axisIndexOf = new Map<string, number>();
  const yAxisIndex: number[] = new Array(series.length).fill(0);

  const byUnit = new Map<string, { index: number; scale: number }[]>();
  series.forEach((s, i) => {
    const key = s.unit ?? '';
    const list = byUnit.get(key) ?? [];
    list.push({ index: i, scale: seriesMaxAbs(s.points) });
    byUnit.set(key, list);
  });

  for (const [unitKey, members] of byUnit) {
    // Greedy magnitude buckets: iterate scales descending, join the first bucket
    // whose max scale is within MAX_AXIS_SCALE_RATIO of this series' scale.
    const buckets: number[] = []; // max scale per bucket
    const bucketOf = new Map<number, number>();
    const sorted = [...members].sort((a, b) => b.scale - a.scale);
    for (const m of sorted) {
      const scale = m.scale > 0 ? m.scale : 1; // empty/zero series join a bucket
      let bucketIdx = buckets.findIndex((max) => max / scale <= MAX_AXIS_SCALE_RATIO);
      if (bucketIdx < 0) {
        bucketIdx = buckets.length;
        buckets.push(scale);
      } else {
        buckets[bucketIdx] = Math.max(buckets[bucketIdx], scale);
      }
      bucketOf.set(m.index, bucketIdx);
    }
    // Renumber magnitude buckets by first-encounter order so `#n` ids line up
    // with axis order (axis 0 = left), regardless of sort order used above.
    const renamed = new Map<number, number>();
    let nextBucket = 0;
    for (const m of members) {
      const b = bucketOf.get(m.index) ?? 0;
      if (!renamed.has(b)) renamed.set(b, nextBucket++);
    }
    // Assign axes in original series order so encounter order stays stable.
    for (const m of members) {
      const bucketIdx = renamed.get(bucketOf.get(m.index) ?? 0) ?? 0;
      const key = buckets.length > 1 ? `${unitKey}#${bucketIdx}` : unitKey;
      let axIdx = axisIndexOf.get(key);
      if (axIdx === undefined) {
        axIdx = axes.length;
        axes.push({ key, label: unitKey });
        axisIndexOf.set(key, axIdx);
      }
      yAxisIndex[m.index] = axIdx;
    }
  }
  return { axes, yAxisIndex };
}

export function buildChartOption(input: ChartOptionInput): EChartsOption {
  const { series, window, cursorMs, colors } = input;
  if (series.length === 0) return emptySeriesOption(window);
  const { axes, yAxisIndex } = resolveAxes(series);

  const markLine =
    cursorMs === null
      ? undefined
      : {
          symbol: 'none',
          silent: true,
          lineStyle: { color: colors?.cursor, width: 1 },
          // P4: markLine label defaults to show:true in ECharts 6 and would render
          // the raw epoch ms; format it like the toolbar's `游标: 08:00:09.945`.
          label: { formatter: () => formatTime(cursorMs) },
          data: [{ xAxis: cursorMs }],
        };

  const option: EChartsOption = {
    animation: false,
    tooltip: { trigger: 'axis' },
    legend: {
      type: 'scroll',
      top: 0,
      textStyle: { color: colors?.textPrimary },
    },
    // P6: each extra right-hand Y axis is shifted RIGHT_AXIS_GAP further out via
    // `offset`, so stacked axes' labels/names don't overlap; reserve margin for them.
    grid: {
      left: RIGHT_AXIS_MARGIN,
      right: RIGHT_AXIS_MARGIN + Math.max(0, axes.length - 2) * RIGHT_AXIS_GAP,
      top: 32,
      bottom: 56,
      borderColor: colors?.border,
    },
    xAxis: {
      type: 'time',
      min: window.t0_ms,
      max: window.t1_ms,
      axisLine: { lineStyle: { color: colors?.textSecondary } },
      axisLabel: { color: colors?.textSecondary },
      axisPointer: {
        type: 'line',
        snap: true,
        lineStyle: { color: colors?.cursor },
        // P4: the tooltip header for `trigger: 'axis'` is rendered through the
        // x-axis axisPointer label formatter (ECharts axisTrigger→TooltipView);
        // this one formatter unifies the axisPointer label AND the tooltip time.
        label: { formatter: (params) => formatTime(Number(params.value)) },
      },
      splitLine: { lineStyle: { color: colors?.grid } },
    },
    yAxis: axes.map((ax, i) => ({
      type: 'value',
      id: ax.key,
      name: ax.label === '' ? undefined : ax.label,
      position: i === 0 ? 'left' : 'right',
      offset: i === 0 ? 0 : (i - 1) * RIGHT_AXIS_GAP,
      axisLabel: { color: colors?.textSecondary },
      // Grid lines from the left axis only: independent scales on stacked right
      // axes would otherwise draw overlapping/duplicate horizontal lines.
      splitLine: { show: i === 0, lineStyle: { color: colors?.grid } },
    })),
    dataZoom: [
      { type: 'inside', xAxisIndex: 0, startValue: window.t0_ms, endValue: window.t1_ms },
      { type: 'slider', xAxisIndex: 0, startValue: window.t0_ms, endValue: window.t1_ms, bottom: 0 },
    ],
    series: series.map((s, i) => ({
      ...SERIES_BASE,
      name: s.name,
      yAxisIndex: yAxisIndex[i],
      color: colors?.series?.[i % (colors.series.length || 1)],
      data: s.points.map((p) => [p.t_ms, p.v] as [number, number]),
      ...(cursorMs !== null ? { markLine } : {}),
    })),
  };
  return option;
}
