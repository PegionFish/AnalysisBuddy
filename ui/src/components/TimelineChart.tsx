/** ui/src/components/TimelineChart.tsx — central ECharts multi-series line chart (ipc-ui.md §4.4/§5).
 *  Fixed high-performance config (ui/src/chart/options.ts); dataZoom = re-query (debounced, seq-guarded by the
 *  session provider); cursor markLine follows cursorMs; theme colors re-read via getComputedStyle. */

import { useEffect, useMemo, useRef } from 'react';
import * as echarts from 'echarts';
import { useTranslation } from 'react-i18next';
import {
  buildChartOption,
  readChartColors,
  resolveChartSeries,
  type ChartThemeColors,
} from '../chart/options';
import { formatTime } from '../lib/format';
import { useSession } from '../state/session';
import './TimelineChart.css';

/** dataZoom end → window dispatch debounce (ipc-ui.md §4.4: 150ms trailing). */
const ZOOM_DEBOUNCE_MS = 150;

/** Structural view of the ECharts instance the component needs (kept minimal, version-agnostic). */
interface ZrLike {
  on(type: string, cb: (params: unknown) => void): void;
}

interface ChartInstanceLike {
  setOption(option: unknown, opts?: { notMerge?: boolean }): void;
  on(type: string, cb: (params: unknown) => void): void;
  dispose(): void;
  /** zrender 底层事件入口（任务 23：series 级 click 在 large+symbol:none 下永不触发）。 */
  getZr(): ZrLike;
  /** 像素坐标是否在绘图网格内（任务 23：网格外点击不设游标）。 */
  containPixel(finder: { gridIndex: number }, point: [number, number]): boolean;
  /** 像素 → 数据坐标反算（'grid' finder → [x 轴值, y 轴值]，x 轴为 UTC 毫秒）。 */
  convertFromPixel(finder: string, pixel: [number, number]): number[] | number;
}

/** Central timeline chart: ECharts multi-series, dataZoom re-query, cursor markLine (ipc-ui.md §4.4). */
export default function TimelineChart() {
  const { state, dispatch, actions } = useSession();
  const { t } = useTranslation();
  const chartElRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<ChartInstanceLike | null>(null);
  const windowRef = useRef(state.viewWindow);
  const zoomRef = useRef<{ t0_ms: number; t1_ms: number } | null>(null);
  const zoomTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const hasFiles = state.files.length > 0;

  const resolvedSeries = useMemo(
    () => resolveChartSeries(state.series, state.files, state.metricTree, state.selectedMetrics),
    [state.series, state.files, state.metricTree, state.selectedMetrics],
  );
  const colors: ChartThemeColors = useMemo(() => readChartColors(state.theme), [state.theme]);

  windowRef.current = state.viewWindow;

  const onDataZoomRef = useRef<(params: unknown) => void>(() => undefined);
  onDataZoomRef.current = (params) => {
    const p = params as { start?: number; end?: number };
    if (typeof p.start !== 'number' || typeof p.end !== 'number') return;
    const win = windowRef.current;
    const span = win.t1_ms - win.t0_ms;
    const t0 = Math.max(0, Math.round(win.t0_ms + (span * p.start) / 100));
    const t1 = Math.max(0, Math.round(win.t0_ms + (span * p.end) / 100));
    if (t1 <= t0) return;
    if (t0 === win.t0_ms && t1 === win.t1_ms) return;
    zoomRef.current = { t0_ms: t0, t1_ms: t1 };
    if (zoomTimerRef.current !== null) clearTimeout(zoomTimerRef.current);
    zoomTimerRef.current = setTimeout(() => {
      const z = zoomRef.current;
      if (z) dispatch({ type: 'chart/window', t0_ms: z.t0_ms, t1_ms: z.t1_ms });
      zoomTimerRef.current = null;
    }, ZOOM_DEBOUNCE_MS);
  };

  /* 任务 23（根因修复）：series 级 chart.on('click') 只在命中数据点图形时触发；
   * SERIES_BASE 为 large:true + symbol:'none'（§5.1 固定性能配置）→ 无可命中目标，
   * 游标永不设置、key_values_at 对用户不可达。改用 zrender 层 click +
   * containPixel(grid) 守卫 + convertFromPixel 反算：网格内任意位置点击即设游标，
   * 不依赖 symbol 命中，大数据量 large 模式同样可用；缩放后换算仍正确
   * （convertFromPixel 以当前坐标系为准）。 */
  const onZrClickRef = useRef<(params: unknown) => void>(() => undefined);
  onZrClickRef.current = (params) => {
    const chart = chartRef.current;
    if (!chart) return;
    const e = params as { offsetX?: number; offsetY?: number };
    if (typeof e.offsetX !== 'number' || typeof e.offsetY !== 'number') return;
    try {
      if (!chart.containPixel({ gridIndex: 0 }, [e.offsetX, e.offsetY])) return;
      const converted = chart.convertFromPixel('grid', [e.offsetX, e.offsetY]);
      const ms = Array.isArray(converted) ? converted[0] : converted;
      if (!Number.isFinite(ms)) return;
      dispatch({ type: 'cursor/set', ms: Math.round(ms) });
    } catch (err) {
      // 环境异常（坐标系未就绪等）不得升级为整树卸载（任务 17 防线延续）。
      console.error('[chart] cursor click failed', err);
    }
  };

  useEffect(() => {
    const el = chartElRef.current;
    if (!el) return;
    // 防御：ECharts init/on 抛错（渲染环境异常等）不得升级为整树卸载（任务 17）。
    try {
      const chart = echarts.init(el);
      chartRef.current = chart;
      chart.on('datazoom', (p) => onDataZoomRef.current(p));
      // 任务 23：zrender 层 click（series 级 click 在 large+symbol:none 下从不触发）。
      chart.getZr().on('click', (p) => onZrClickRef.current(p));
    } catch (e) {
      console.error('[chart] init failed', e);
      chartRef.current = null;
    }
    return () => {
      try {
        chartRef.current?.dispose();
      } catch (e) {
        console.error('[chart] dispose failed', e);
      }
      chartRef.current = null;
    };
  }, [hasFiles]);

  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;
    // 防御：setOption 抛错（option 形状/环境异常）不得升级为整树卸载（任务 17）。
    try {
      chart.setOption(
        buildChartOption({
          series: resolvedSeries,
          window: state.viewWindow,
          cursorMs: state.cursorMs,
          colors,
        }),
        { notMerge: true },
      );
    } catch (e) {
      console.error('[chart] setOption failed', e);
    }
  }, [hasFiles, resolvedSeries, state.viewWindow, state.cursorMs, colors]);

  if (!hasFiles) {
    return (
      <section className="panel timeline-chart timeline-chart--empty">
        <h3 className="timeline-chart__empty-title">{t('workbench.chart.empty_title')}</h3>
        <p className="timeline-chart__empty-body">{t('workbench.chart.empty_body')}</p>
      </section>
    );
  }

  const anyDownsampled = resolvedSeries.some((s) => s.downsampled);

  return (
    <section className="panel timeline-chart">
      <header className="timeline-chart__toolbar">
        {state.cursorMs !== null && (
          <span className="timeline-chart__cursor" data-testid="chart-cursor">
            {t('workbench.chart.cursor', { time: formatTime(state.cursorMs) })}
          </span>
        )}
        {anyDownsampled && (
          <span className="timeline-chart__badge" data-testid="chart-downsampled">
            {t('workbench.chart.downsampled')}
          </span>
        )}
        <button
          type="button"
          className="timeline-chart__btn"
          data-testid="chart-zoom-reset"
          /* 任务 19：适配当前数据时间域并集，而非固定 INITIAL_VIEW_WINDOW（epoch 0）。 */
          onClick={() => actions.fitViewToData()}
        >
          {t('workbench.chart.zoom_reset')}
        </button>
      </header>
      <div className="timeline-chart__body">
        <div
          ref={chartElRef}
          className="timeline-chart__canvas"
          role="img"
          aria-label={t('workbench.chart.legend')}
          data-testid="timeline-chart-canvas"
        />
        {state.selectedMetrics.size === 0 && (
          <div className="timeline-chart__hint" data-testid="chart-hint">
            {t('workbench.chart.no_metrics')}
          </div>
        )}
      </div>
    </section>
  );
}
