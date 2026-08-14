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

describe('sessionReducer presets/apply (preset apply atomic replace)', () => {
  it('replaces selectedMetrics with the exact set and keeps series for a non-empty selection', () => {
    const base = sessionReducer(initialSessionState(), {
      type: 'metrics/toggle',
      ids: ['f1:p1:m1'],
      checked: true,
    });
    const withSeries = sessionReducer(base, { type: 'chart/series', series: [slice('m1', 1)], seq: 1 });

    const applied = sessionReducer(withSeries, {
      type: 'presets/apply',
      selected: ['f1:p2:m2', 'f2:p1:m3'],
    });

    expect(applied.selectedMetrics).toEqual(new Set(['f1:p2:m2', 'f2:p1:m3']));
    // 非空替换：series 保留，由 query effect 的 seq 机制收敛晚到响应。
    expect(applied.series).toEqual([slice('m1', 1)]);
  });

  it('clears selection and series when applied with an empty array', () => {
    const base = sessionReducer(initialSessionState(), {
      type: 'metrics/toggle',
      ids: ['f1:p1:m1', 'f2:p1:m2'],
      checked: true,
    });
    const withSeries = sessionReducer(base, { type: 'chart/series', series: [slice('m1', 1)], seq: 1 });

    const cleared = sessionReducer(withSeries, { type: 'presets/apply', selected: [] });

    expect(cleared.selectedMetrics.size).toBe(0);
    // P1-04 语义：全取消 → 曲线清空。
    expect(cleared.series).toEqual([]);
  });

  it('replaces the previous selection wholesale instead of unioning', () => {
    const base = sessionReducer(initialSessionState(), {
      type: 'metrics/toggle',
      ids: ['f1:p1:old1', 'f1:p1:old2'],
      checked: true,
    });

    const applied = sessionReducer(base, { type: 'presets/apply', selected: ['f1:p1:new1'] });

    // 旧 Set 被整体替换而非并集：旧 id 不得存活，且为新 Set 实例。
    expect(applied.selectedMetrics).toEqual(new Set(['f1:p1:new1']));
    expect(applied.selectedMetrics).not.toBe(base.selectedMetrics);
    expect([...applied.selectedMetrics].filter((id) => id.includes('old'))).toEqual([]);
  });
});
