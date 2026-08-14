/** ui/src/lib/presetMatch.ts — 场景预设前端匹配引擎（纯函数，无副作用）。
 *  契约：Preset_场景预设功能.md「数据模型/匹配三级」+ Wave 3 C7 冻结包。
 *  三级匹配：① 穷举精确（候选先 metric_id ===，再 name 大小写不敏感相等；
 *  同 want 仅首个命中生效，命中记录分组）；② keywords 子串模糊兜底（仅穷举
 *  整体零命中时启用，matchedBy='fuzzy'）；③ 仍零命中 → selected=[]，
 *  unmatched 清单化提示（组件不应用）。 */

import type { MetricNode, PresetDef, PresetEntry, UserPreset } from '../ipc/types';

/** 匹配方式：'exact' 穷举精确 | 'fuzzy' keywords 子串兜底。 */
export type PresetMatchedBy = 'exact' | 'fuzzy';

/** 单条命中（UI 来源标记/结果 toast 用）。 */
export interface PresetHit {
  /** 命中条目的 want（无则省略）。 */
  want?: string;
  /** 命中分组 id（顶层条目 = null）。 */
  groupId: string | null;
  /** 命中指标复合 id（`file_id:plugin_id:metric_id`）。 */
  compositeId: string;
  /** 命中方式。 */
  matchedBy: PresetMatchedBy;
  /** 命中的候选名原文（展示用）。 */
  matchedName: string;
}

/** 零命中条目清单（清单化提示 unmatched）。 */
export interface PresetUnmatched {
  want?: string;
  groupId: string | null;
  names: string[];
}

/** 匹配结果。 */
export interface PresetMatchResult {
  /** 命中的复合 id 列表（去重，保序）。 */
  selected: string[];
  hits: PresetHit[];
  unmatched: PresetUnmatched[];
}

/** 匹配域：metricTree 中 level==='file' 节点的 children（level==='plugin' 且
 *  plugin_id===目标）的 children（level==='metric'），按文件顺序平铺。 */
function collectMetrics(pluginId: string, metricTree: MetricNode[]): MetricNode[] {
  const domain: MetricNode[] = [];
  for (const file of metricTree) {
    if (file.level !== 'file') continue;
    for (const plugin of file.children ?? []) {
      if (plugin.level !== 'plugin' || plugin.plugin_id !== pluginId) continue;
      for (const metric of plugin.children ?? []) {
        if (metric.level === 'metric') domain.push(metric);
      }
    }
  }
  return domain;
}

/** 穷举精确：候选先对匹配域全部 metric_id 做 ===（大小写敏感），未命中再对
 *  全部 name 做 toLowerCase 相等（大小写不敏感）；返回首个命中节点。 */
function findExact(domain: MetricNode[], candidate: string): MetricNode | undefined {
  const byId = domain.find((metric) => metric.metric_id === candidate);
  if (byId) return byId;
  const lower = candidate.toLowerCase();
  return domain.find((metric) => metric.name.toLowerCase() === lower);
}

/**
 * 插件预设三级匹配：
 * ① 穷举精确：遍历顶层 entries + 各 group.entries；names 候选先在
 *    metric_id 精确匹配（===），未命中再按 name 大小写不敏感匹配
 *    （toLowerCase 相等）；同 want 仅首个命中生效；命中记录 groupId；
 * ② 若整体零命中（selected 为空）：keywords 对 metric_id/name 做
 *    子串匹配（includes），命中全选，matchedBy='fuzzy'；
 * ③ 仍零命中：selected=[]，unmatched 返回全部条目（组件不应用）。
 * metricTree 为三棵树（file → plugin → metric）；只匹配 pluginId 的节点。
 */
export function matchPreset(
  preset: PresetDef,
  pluginId: string,
  metricTree: MetricNode[],
): PresetMatchResult {
  const domain = collectMetrics(pluginId, metricTree);
  const selected: string[] = [];
  const selectedSet = new Set<string>();
  const hits: PresetHit[] = [];
  const unmatched: PresetUnmatched[] = [];
  /** 已命中过的 want：后续同 want 条目整体跳过（不命中、不进 unmatched）。 */
  const wantSeen = new Set<string>();

  const processEntry = (entry: PresetEntry, groupId: string | null): void => {
    if (entry.want !== undefined && wantSeen.has(entry.want)) return;
    for (const candidate of entry.names) {
      const node = findExact(domain, candidate);
      if (!node) continue;
      if (entry.want !== undefined) wantSeen.add(entry.want);
      hits.push({ want: entry.want, groupId, compositeId: node.id, matchedBy: 'exact', matchedName: candidate });
      if (!selectedSet.has(node.id)) {
        selectedSet.add(node.id);
        selected.push(node.id);
      }
      return;
    }
    unmatched.push({ want: entry.want, groupId, names: [...entry.names] });
  };

  for (const entry of preset.entries ?? []) processEntry(entry, null);
  for (const group of preset.groups ?? []) {
    for (const entry of group.entries ?? []) processEntry(entry, group.id);
  }

  if (selected.length === 0) {
    for (const keyword of preset.keywords ?? []) {
      const lower = keyword.toLowerCase();
      for (const metric of domain) {
        if (selectedSet.has(metric.id)) continue;
        const metricId = metric.metric_id ?? '';
        if (metricId.toLowerCase().includes(lower) || metric.name.toLowerCase().includes(lower)) {
          selectedSet.add(metric.id);
          selected.push(metric.id);
          hits.push({ compositeId: metric.id, groupId: null, matchedBy: 'fuzzy', matchedName: keyword });
        }
      }
    }
  }

  return { selected, hits, unmatched };
}

/** 用户预设匹配：对 entries 每键（plugin_id）按该插件 metric 穷举精确
 * 匹配（无模糊兜底——天然精确）；全零命中 → selected=[]。 */
export function matchUserPreset(
  preset: UserPreset,
  metricTree: MetricNode[],
): PresetMatchResult {
  const selected: string[] = [];
  const selectedSet = new Set<string>();
  const hits: PresetHit[] = [];
  const unmatched: PresetUnmatched[] = [];

  for (const [pluginId, metricIds] of Object.entries(preset.entries)) {
    const pluginNodes: MetricNode[] = [];
    for (const file of metricTree) {
      if (file.level !== 'file') continue;
      for (const plugin of file.children ?? []) {
        if (plugin.level === 'plugin' && plugin.plugin_id === pluginId) pluginNodes.push(plugin);
      }
    }
    let anyHit = false;
    for (const metricId of metricIds) {
      for (const plugin of pluginNodes) {
        for (const metric of plugin.children ?? []) {
          if (metric.level !== 'metric' || metric.metric_id !== metricId) continue;
          anyHit = true;
          hits.push({ groupId: pluginId, compositeId: metric.id, matchedBy: 'exact', matchedName: metricId });
          if (!selectedSet.has(metric.id)) {
            selectedSet.add(metric.id);
            selected.push(metric.id);
          }
        }
      }
    }
    if (!anyHit) unmatched.push({ groupId: pluginId, names: [...metricIds] });
  }

  return { selected, hits, unmatched };
}

/** 从 selectedMetrics（`file_id:plugin_id:metric_id`）反推用户预设
 * entries（plugin_id → metric_id 去重列表；畸形复合 id 忽略）。 */
export function deriveUserPresetEntries(selectedMetrics: Iterable<string>): Record<string, string[]> {
  const entries: Record<string, string[]> = {};
  for (const composite of selectedMetrics) {
    const parts = composite.split(':');
    if (parts.length !== 3) continue;
    const [, pluginId, metricId] = parts;
    const list = entries[pluginId] ?? [];
    if (!list.includes(metricId)) list.push(metricId);
    entries[pluginId] = list;
  }
  return entries;
}
