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
import { INITIAL_VIEW_WINDOW, useSession } from '../state/session';
import './TimelineChart.css';

/** dataZoom end → window dispatch debounce (ipc-ui.md §4.4: 150ms trailing). */
const ZOOM_DEBOUNCE_MS = 150;

/** Structural view of the ECharts instance the component needs (kept minimal, version-agnostic). */
interface ChartInstanceLike {
  setOption(option: unknown, opts?: { notMerge?: boolean }): void;
  on(type: string, cb: (params: unknown) => void): void;
  dispose(): void;
}

/** Central timeline chart: ECharts multi-series, dataZoom re-query, cursor markLine (ipc-ui.md §4.4). */
export default function TimelineChart() {
  const { state, dispatch } = useSession();
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

  const onClickRef = useRef<(params: unknown) => void>(() => undefined);
  onClickRef.current = (params) => {
    const p = params as { value?: unknown };
    const raw = Array.isArray(p.value) ? p.value[0] : p.value;
    if (typeof raw === 'number' && Number.isFinite(raw)) {
      dispatch({ type: 'cursor/set', ms: Math.round(raw) });
    }
  };

  useEffect(() => {
    const el = chartElRef.current;
    if (!el) return;
    const chart = echarts.init(el);
    chartRef.current = chart;
    chart.on('datazoom', (p) => onDataZoomRef.current(p));
    chart.on('click', (p) => onClickRef.current(p));
    return () => {
      chart.dispose();
      chartRef.current = null;
    };
  }, [hasFiles]);

  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;
    chart.setOption(
      buildChartOption({
        series: resolvedSeries,
        window: state.viewWindow,
        cursorMs: state.cursorMs,
        colors,
      }),
      { notMerge: true },
    );
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
          onClick={() =>
            dispatch({ type: 'chart/window', t0_ms: INITIAL_VIEW_WINDOW.t0_ms, t1_ms: INITIAL_VIEW_WINDOW.t1_ms })
          }
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
