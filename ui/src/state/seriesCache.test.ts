import { describe, expect, it } from 'vitest';
import type { SeriesSlice } from '../ipc/types';
import {
  SERIES_CACHE_MAX_ENTRIES,
  SeriesCache,
  seriesCacheKey,
} from './seriesCache';

function slice(file_id: string, metric_id: string, v = 1): SeriesSlice {
  const base: SeriesSlice = {
    file_id,
    plugin_id: 'mock',
    metric_id,
    point_count: 1,
    downsampled: false,
    points: [{ t_ms: 0, v }],
  };
  // F5 wire 形态（additive 运行时键）：模拟新服务端响应里的列式扩展。
  return Object.assign(base, { format: 'columnar', ts: [0], values: [v] });
}

const BUDGET = 4000;

describe('seriesCache (F5: query_result memo keyed by file+metric+window+budget)', () => {
  it('returns the identical slice object for the same key (zero-rebuild reuse)', () => {
    const cache = new SeriesCache();
    const s = slice('f1', 'fps');
    cache.set(seriesCacheKey('f1', 'fps', 0, 600_000, BUDGET), s);
    const hit = cache.get(seriesCacheKey('f1', 'fps', 0, 600_000, BUDGET));
    expect(hit).toBe(s);
  });

  it('distinguishes every key dimension: file, metric, window and budget', () => {
    const cache = new SeriesCache();
    cache.set(seriesCacheKey('f1', 'fps', 0, 600_000, BUDGET), slice('f1', 'fps', 1));
    expect(cache.get(seriesCacheKey('f2', 'fps', 0, 600_000, BUDGET))).toBeUndefined();
    expect(cache.get(seriesCacheKey('f1', 'mem', 0, 600_000, BUDGET))).toBeUndefined();
    expect(cache.get(seriesCacheKey('f1', 'fps', 1, 600_000, BUDGET))).toBeUndefined();
    expect(cache.get(seriesCacheKey('f1', 'fps', 0, 599_999, BUDGET))).toBeUndefined();
    expect(cache.get(seriesCacheKey('f1', 'fps', 0, 600_000, BUDGET + 1))).toBeUndefined();
  });

  it('invalidateFile drops only that file\u2019s entries (any window/budget)', () => {
    const cache = new SeriesCache();
    cache.set(seriesCacheKey('f1', 'fps', 0, 600_000, BUDGET), slice('f1', 'fps'));
    cache.set(seriesCacheKey('f1', 'fps', 1000, 2000, BUDGET), slice('f1', 'fps'));
    cache.set(seriesCacheKey('f12', 'fps', 0, 600_000, BUDGET), slice('f12', 'fps'));
    cache.invalidateFile('f1');
    expect(cache.get(seriesCacheKey('f1', 'fps', 0, 600_000, BUDGET))).toBeUndefined();
    expect(cache.get(seriesCacheKey('f1', 'fps', 1000, 2000, BUDGET))).toBeUndefined();
    // f12 与 f1 是不同键（值内闭合引号保证不误伤前缀相似 id）。
    expect(cache.get(seriesCacheKey('f12', 'fps', 0, 600_000, BUDGET))).toBeDefined();
  });

  it('clear() empties everything (session boundary / plugin reload semantics)', () => {
    const cache = new SeriesCache();
    cache.set(seriesCacheKey('f1', 'fps', 0, 600_000, BUDGET), slice('f1', 'fps'));
    cache.set(seriesCacheKey('f2', 'fps', 0, 600_000, BUDGET), slice('f2', 'fps'));
    expect(cache.size).toBe(2);
    cache.clear();
    expect(cache.size).toBe(0);
    expect(cache.get(seriesCacheKey('f1', 'fps', 0, 600_000, BUDGET))).toBeUndefined();
  });

  it('evicts the oldest entry FIFO once SERIES_CACHE_MAX_ENTRIES is reached', () => {
    const cache = new SeriesCache();
    const oldestKey = seriesCacheKey('f0', 'fps', 0, 600_000, BUDGET);
    cache.set(oldestKey, slice('f0', 'fps'));
    for (let i = 1; cache.size < SERIES_CACHE_MAX_ENTRIES; i++) {
      cache.set(seriesCacheKey(`f${i}`, 'fps', 0, 600_000, BUDGET), slice(`f${i}`, 'fps'));
    }
    expect(cache.size).toBe(SERIES_CACHE_MAX_ENTRIES);
    expect(cache.get(oldestKey)).toBeDefined();
    // 再放一条 → 最旧的 f0 被逐出，容量不增长。
    cache.set(seriesCacheKey('f-new', 'fps', 0, 600_000, BUDGET), slice('f-new', 'fps'));
    expect(cache.size).toBe(SERIES_CACHE_MAX_ENTRIES);
    expect(cache.get(oldestKey)).toBeUndefined();
    expect(cache.get(seriesCacheKey('f-new', 'fps', 0, 600_000, BUDGET))).toBeDefined();
  });

  it('overwriting an existing key does not evict or grow the cache', () => {
    const cache = new SeriesCache();
    const key = seriesCacheKey('f1', 'fps', 0, 600_000, BUDGET);
    cache.set(key, slice('f1', 'fps', 1));
    cache.set(key, slice('f1', 'fps', 2));
    expect(cache.size).toBe(1);
    expect(cache.get(key)?.points[0].v).toBe(2);
  });
});
