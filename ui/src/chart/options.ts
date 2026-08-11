/** ui/src/chart/options.ts — the single chart-option entry point (ipc-ui.md §5.1 fixed config).
 *  TimelineChart never builds ECharts options inline; everything goes through buildChartOption. */

import type { EChartsOption } from 'echarts';
import type { MetricNode, SeriesPoint, SeriesSlice, Theme } from '../ipc/types';

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

export function buildChartOption(input: ChartOptionInput): EChartsOption {
  const { series, window, cursorMs, colors } = input;
  if (series.length === 0) return emptySeriesOption(window);
  const units: string[] = [];
  const unitIndex = (unit: string | undefined): number => {
    const key = unit ?? '';
    const existing = units.indexOf(key);
    if (existing >= 0) return existing;
    units.push(key);
    return units.length - 1;
  };

  const markLine =
    cursorMs === null
      ? undefined
      : {
          symbol: 'none',
          silent: true,
          lineStyle: { color: colors?.cursor, width: 1 },
          data: [{ xAxis: cursorMs }],
        };
  const yAxisIndex = series.map((s) => unitIndex(s.unit));

  const option: EChartsOption = {
    animation: false,
    tooltip: { trigger: 'axis' },
    legend: {
      type: 'scroll',
      top: 0,
      textStyle: { color: colors?.textPrimary },
    },
    grid: { left: 56, right: 56, top: 32, bottom: 56, borderColor: colors?.border },
    xAxis: {
      type: 'time',
      min: window.t0_ms,
      max: window.t1_ms,
      axisLine: { lineStyle: { color: colors?.textSecondary } },
      axisLabel: { color: colors?.textSecondary },
      axisPointer: { type: 'line', snap: true, lineStyle: { color: colors?.cursor } },
      splitLine: { lineStyle: { color: colors?.grid } },
    },
    yAxis: units.map((unit, i) => ({
      type: 'value',
      id: unit,
      name: unit === '' ? undefined : unit,
      position: i === 0 ? 'left' : 'right',
      axisLabel: { color: colors?.textSecondary },
      splitLine: { lineStyle: { color: colors?.grid } },
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
