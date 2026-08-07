import { describe, expect, it } from 'vitest';
import type { KeyValueResult, SeriesSlice } from '../ipc/types';
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

function kvResult(fileId: string, mark: string): KeyValueResult {
  return { file_id: fileId, entries: [{ key: 'mark', value: mark }] };
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

describe('sessionReducer key-values wiring (ipc-ui.md §5.3)', () => {
  it('keyvalues/set drops stale seq results and clears the pending flag', () => {
    let state = sessionReducer(initialSessionState(), { type: 'keyvalues/pending', pending: true });
    expect(state.keyValuesPending).toBe(true);

    state = sessionReducer(state, { type: 'keyvalues/set', results: [kvResult('f1', 'new')], seq: 2 });
    expect(state.keyValues).toEqual([kvResult('f1', 'new')]);
    expect(state.keyValuesSeq).toBe(2);
    expect(state.keyValuesPending).toBe(false);

    const stale = sessionReducer(state, { type: 'keyvalues/set', results: [kvResult('f1', 'old')], seq: 1 });
    expect(stale.keyValues).toEqual([kvResult('f1', 'new')]);
    expect(stale.keyValuesSeq).toBe(2);
  });

  it('keyvalues/merge applies only while the latest accepted query is current', () => {
    const base = sessionReducer(initialSessionState(), { type: 'keyvalues/set', results: [kvResult('f1', 'a')], seq: 1 });
    expect(base.keyValuesSeq).toBe(1);

    const merged = sessionReducer(base, { type: 'keyvalues/merge', results: [kvResult('f1', 'b')], seq: 1 });
    expect(merged.keyValues).toEqual([kvResult('f1', 'b')]);

    const newer = sessionReducer(merged, { type: 'keyvalues/set', results: [kvResult('f1', 'c')], seq: 2 });
    const staleMerge = sessionReducer(newer, { type: 'keyvalues/merge', results: [kvResult('f1', 'x')], seq: 1 });
    expect(staleMerge.keyValues).toEqual([kvResult('f1', 'c')]);
  });

  it('keyvalues/merge upserts per-file results without touching other files', () => {
    const base = sessionReducer(initialSessionState(), {
      type: 'keyvalues/set',
      results: [kvResult('f1', 'a'), kvResult('f2', 'b')],
      seq: 1,
    });
    const merged = sessionReducer(base, { type: 'keyvalues/merge', results: [kvResult('f2', 'b2')], seq: 1 });
    expect(merged.keyValues).toEqual([kvResult('f1', 'a'), kvResult('f2', 'b2')]);
  });
});

describe('sessionReducer plugin health (ipc-ui.md §2.3/§4.6)', () => {
  it('plugins/health flips state and records last_error detail', () => {
    const base = sessionReducer(initialSessionState(), {
      type: 'plugins/set',
      plugins: [
        {
          id: 'p1',
          display_name: 'P1',
          version: '1.0.0',
          state: 'ready',
          loaded_file_ids: [],
          capabilities: { annotate: false, subscribe: false, binary_sidecar: false },
          last_error: null,
        },
      ],
    });
    const crashed = sessionReducer(base, {
      type: 'plugins/health',
      payload: { plugin_id: 'p1', state: 'crashed', prev_state: 'ready', detail: 'exit code 1' },
    });
    expect(crashed.plugins[0].state).toBe('crashed');
    expect(crashed.plugins[0].last_error).toBe('exit code 1');

    const readyAgain = sessionReducer(crashed, {
      type: 'plugins/health',
      payload: { plugin_id: 'p1', state: 'ready', prev_state: 'crashed' },
    });
    expect(readyAgain.plugins[0].state).toBe('ready');
    expect(readyAgain.plugins[0].last_error).toBeNull();
  });
});
