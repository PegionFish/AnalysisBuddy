/** ui/src/state/seriesCache.ts — F5 查询结果浅缓存。
 *
 *  键 = (file_id, metric_id, t0_ms, t1_ms, 预算)：同键的 query_series 切片
 *  直接复用（同一 wire 对象引用），免去重复 IPC 往返与列式/成对数据的全量重建
 *  （options.ts 的 WeakMap 记忆按切片对象身份命中，引用不变即零重建）。
 *
 *  失效语义（唯一权威）：
 *  - `invalidateFile(file_id)`：文件卸载（unloadFile / files/unloaded 路径）；
 *  - `clear()`：跨会话边界（新建/打开会话全量卸载）与数据可能变化的插件生命周期
 *    事件（reload/update 重经导入管线）。
 *  缓存仅持有 Frozen 文件的确定性查询结果（同窗口+同预算 → 同输出），浅 Map、
 *  FIFO 限界（MAX_ENTRIES），不感知 React 生命周期。 */

import type { SeriesSlice } from '../ipc/types';

/** 缓存条目上限（FIFO 逐出最旧）：视口连续缩放会派生大量窗口键，限界防漂移。 */
export const SERIES_CACHE_MAX_ENTRIES = 64;

/** 缓存键（JSON 序列化保证无分隔符歧义；metric_id 不含冒号由引擎 §1.5 解析保证，
 *  JSON 键仍显式带字段，防御 id 形状变化）。 */
export type SeriesCacheKey = {
  file_id: string;
  metric_id: string;
  t0_ms: number;
  t1_ms: number;
  max_points_per_series: number;
};

export function seriesCacheKey(
  file_id: string,
  metric_id: string,
  t0_ms: number,
  t1_ms: number,
  max_points_per_series: number,
): string {
  const key: SeriesCacheKey = { file_id, metric_id, t0_ms, t1_ms, max_points_per_series };
  return JSON.stringify(key);
}

export class SeriesCache {
  private entries = new Map<string, SeriesSlice>();

  get size(): number {
    return this.entries.size;
  }

  get(key: string): SeriesSlice | undefined {
    return this.entries.get(key);
  }

  set(key: string, slice: SeriesSlice): void {
    if (!this.entries.has(key) && this.entries.size >= SERIES_CACHE_MAX_ENTRIES) {
      // FIFO：Map 迭代序 = 插入序，逐出最旧一条。
      const oldest = this.entries.keys().next();
      if (!oldest.done) this.entries.delete(oldest.value);
    }
    this.entries.set(key, slice);
  }

  /** 按文件失效（卸载该文件后，其任何窗口/预算的结果都不再可信）。 */
  invalidateFile(file_id: string): void {
    for (const key of this.entries.keys()) {
      if (key.includes(`"file_id":"${file_id}"`)) {
        this.entries.delete(key);
      }
    }
  }

  clear(): void {
    this.entries.clear();
  }
}
