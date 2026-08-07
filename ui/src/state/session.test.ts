import { describe, expect, it } from 'vitest';
import type { SeriesSlice } from '../ipc/types';
import { initialSessionState, sessionReducer } from './session';

function slice(metricId: string, mark: number): SeriesSlice {
  return {
    file_id: 'f1',
    plugin_id: 'p1',
    metric_id: metricId,
    point_count: 1,
    downsampled: false,
    points: [{ t_ms: mark, v: mark }],
  };
}

describe('sessionReducer chart wiring (ipc-ui.md §5.2/§5.3)', () => {
  it('chart/window stores the zoomed window', () => {
    const state = sessionReducer(initialSessionState(), { type: 'chart/window', t0_ms: 60_000, t1_ms: 240_000 });
    expect(state.viewWindow).toEqual({ t0_ms: 60_000, t1_ms: 240_000 });
  });

  it('cursor/set stores the cursor position and can clear it', () => {
    let state = sessionReducer(initialSessionState(), { type: 'cursor/set', ms: 123_456 });
    expect(state.cursorMs).toBe(123_456);
    state = sessionReducer(state, { type: 'cursor/set', ms: null });
    expect(state.cursorMs).toBeNull();
  });

  it('drops out-of-order query results: a slow earlier response arriving late is discarded', () => {
    const initial = initialSessionState();
    const withSeq1 = sessionReducer(initial, { type: 'chart/series', series: [slice('m1', 1)], seq: 1 });
    expect(withSeq1.seriesSeq).toBe(1);

    const withSeq2 = sessionReducer(withSeq1, { type: 'chart/series', series: [slice('m1', 2)], seq: 2 });
    expect(withSeq2.series).toEqual([slice('m1', 2)]);

    const lateSeq1 = sessionReducer(withSeq2, { type: 'chart/series', series: [slice('m1', 99)], seq: 1 });
    expect(lateSeq1.seriesSeq).toBe(2);
    expect(lateSeq1.series).toEqual([slice('m1', 2)]);
  });

  it('accepts a result with the current seq when no newer request exists', () => {
    const state = sessionReducer(initialSessionState(), { type: 'chart/series', series: [slice('m1', 5)], seq: 3 });
    expect(state.series).toEqual([slice('m1', 5)]);
    expect(state.seriesSeq).toBe(3);
  });
});
