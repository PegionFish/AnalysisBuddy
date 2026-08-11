/** ui/src/state/sessionViewport.test.ts — 任务 19 视口自动适配纯函数回归：
 *  多文件并集、零跨度/缺失兜底、ready 过滤。渲染链集成用例见
 *  components/viewport-fit.test.tsx。 */

import { describe, expect, it } from 'vitest';
import {
  INITIAL_VIEW_WINDOW,
  MIN_FIT_SPAN_MS,
  fitWindowForRange,
  readyFileTimeRanges,
  unionTimeRange,
} from './session';
import type { ImportResult } from '../ipc/types';

function file(id: string, status: ImportResult['status'], timeRange?: { start_ms: number; end_ms: number }): ImportResult {
  return {
    file_id: id,
    path: `C:\\data\\${id}.csv`,
    name: `${id}.csv`,
    size_bytes: 10,
    status,
    matched_plugin: null,
    candidate_plugins: [],
    time_range: timeRange,
  };
}

describe('task 19: unionTimeRange', () => {
  it('单个范围原样返回', () => {
    expect(unionTimeRange([{ start_ms: 1785600000000, end_ms: 1785603600000 }])).toEqual({
      start_ms: 1785600000000,
      end_ms: 1785603600000,
    });
  });

  it('多文件取并集（min start, max end）', () => {
    expect(
      unionTimeRange([
        { start_ms: 1785600000000, end_ms: 1785603600000 },
        { start_ms: 1785590000000, end_ms: 1785601000000 },
        { start_ms: 1785602000000, end_ms: 1785620000000 },
      ]),
    ).toEqual({ start_ms: 1785590000000, end_ms: 1785620000000 });
  });

  it('空/全 null/非有限值 → null（缺失回落信号）', () => {
    expect(unionTimeRange([])).toBeNull();
    expect(unionTimeRange([null, undefined])).toBeNull();
    expect(unionTimeRange([{ start_ms: Number.NaN, end_ms: 5 }])).toBeNull();
    expect(unionTimeRange([{ start_ms: 1, end_ms: Number.POSITIVE_INFINITY }])).toBeNull();
  });

  it('逐项忽略 null/非法项，保留有效项', () => {
    expect(
      unionTimeRange([null, { start_ms: 100, end_ms: 200 }, undefined, { start_ms: Number.NaN, end_ms: 3 }]),
    ).toEqual({ start_ms: 100, end_ms: 200 });
  });
});

describe('task 19: fitWindowForRange', () => {
  it('正常范围 → 视口 = 数据域', () => {
    expect(fitWindowForRange({ start_ms: 1785600000000, end_ms: 1785603600000 })).toEqual({
      t0_ms: 1785600000000,
      t1_ms: 1785603600000,
    });
  });

  it('缺失（null）→ 回落默认视口', () => {
    expect(fitWindowForRange(null)).toEqual(INITIAL_VIEW_WINDOW);
  });

  it('零跨度（t0==t1）→ 最小兜底窗口（以数据点居中）', () => {
    expect(fitWindowForRange({ start_ms: 1785600000000, end_ms: 1785600000000 })).toEqual({
      t0_ms: 1785600000000 - MIN_FIT_SPAN_MS / 2,
      t1_ms: 1785600000000 + MIN_FIT_SPAN_MS / 2,
    });
  });

  it('反序范围 → 同样给最小兜底窗口', () => {
    expect(fitWindowForRange({ start_ms: 500, end_ms: 100 })).toEqual({
      t0_ms: 500 - MIN_FIT_SPAN_MS / 2,
      t1_ms: 500 + MIN_FIT_SPAN_MS / 2,
    });
  });
});

describe('task 19: readyFileTimeRanges', () => {
  it('仅收 ready 且携带 time_range 的文件', () => {
    const files = [
      file('a', 'ready', { start_ms: 1, end_ms: 2 }),
      file('b', 'parsing', { start_ms: 3, end_ms: 4 }),
      file('c', 'ready'),
      file('d', 'error', { start_ms: 5, end_ms: 6 }),
      file('e', 'ready', { start_ms: 7, end_ms: 8 }),
    ];
    expect(readyFileTimeRanges(files)).toEqual([
      { start_ms: 1, end_ms: 2 },
      { start_ms: 7, end_ms: 8 },
    ]);
  });
});
