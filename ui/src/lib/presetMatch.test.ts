/** ui/src/lib/presetMatch.test.ts — 场景预设匹配引擎全分支单测（Wave 3 C7 冻结包）。
 *  分支：穷举精确（metric_id/name/优先级/跨文件域/分组/want 去重/selected 去重）、
 *  keywords 模糊兜底（零命中才启用/大小写不敏感/去重）、零命中 unmatched 清单、
 *  matchUserPreset（多插件/逐文件/部分命中/全零命中）、deriveUserPresetEntries
 *  （聚合/跨文件去重/畸形 id 忽略/空输入）。 */
import { describe, expect, it } from 'vitest';
import type { MetricNode, UserPreset } from '../ipc/types';
import { deriveUserPresetEntries, matchPreset, matchUserPreset } from './presetMatch';

interface MetricFixture {
  id: string;
  name: string;
}

interface PluginFixture {
  pluginId: string;
  metrics: MetricFixture[];
}

interface FileFixture {
  fileId: string;
  plugins: PluginFixture[];
}

/** 构造 file → plugin → metric 三棵树夹具（复合 id = file_id:plugin_id:metric_id）。 */
function tree(files: FileFixture[]): MetricNode[] {
  return files.map((file) => ({
    level: 'file' as const,
    id: file.fileId,
    file_id: file.fileId,
    name: file.fileId,
    children: file.plugins.map((plugin) => ({
      level: 'plugin' as const,
      id: plugin.pluginId,
      file_id: file.fileId,
      plugin_id: plugin.pluginId,
      name: plugin.pluginId,
      children: plugin.metrics.map((metric) => ({
        level: 'metric' as const,
        id: `${file.fileId}:${plugin.pluginId}:${metric.id}`,
        file_id: file.fileId,
        plugin_id: plugin.pluginId,
        metric_id: metric.id,
        name: metric.name,
      })),
    })),
  }));
}

/** 构造 PresetEntry 夹具（want 可省略）。 */
function entry(names: string[], want?: string): { want?: string; names: string[] } {
  return want === undefined ? { names } : { want, names };
}

/** 构造 PresetGroup 夹具。 */
function group(id: string, entries: Array<{ want?: string; names: string[] }>) {
  return { id, name: { zh: id, en: id }, entries };
}

/** 构造 PresetDef 夹具（id/name 固定；形状即契约，调用点按 PresetDef 校验）。 */
function preset(
  entries: Array<{ want?: string; names: string[] }> = [],
  groups: Array<{ id: string; name: { zh: string; en: string }; entries: Array<{ want?: string; names: string[] }> }> = [],
  keywords: string[] = [],
) {
  return { id: 'p1', name: { zh: '预设', en: 'Preset' }, entries, groups, keywords };
}

/** 构造 UserPreset 夹具。 */
function userPreset(entries: Record<string, string[]>) {
  return { id: 'up1', name: { zh: '用户预设', en: 'User Preset' }, entries };
}

/** 基准树：f1 有 perf/gpu 两插件，f2 只有 perf——构成多文件多插件匹配域。 */
const baseTree: MetricNode[] = tree([
  {
    fileId: 'f1',
    plugins: [
      {
        pluginId: 'perf',
        metrics: [
          { id: 'fps', name: 'fps' },
          { id: 'frame_time', name: 'Frame Time' },
          { id: 'cpu_usage', name: 'CPU Usage' },
        ],
      },
      {
        pluginId: 'gpu',
        metrics: [
          { id: 'fps', name: 'FPS' },
          { id: 'gpu_usage', name: 'GPU Usage' },
        ],
      },
    ],
  },
  {
    fileId: 'f2',
    plugins: [
      {
        pluginId: 'perf',
        metrics: [
          { id: 'fps', name: 'fps' },
          { id: 'cpu_temp', name: 'CPU Temperature' },
        ],
      },
    ],
  },
]);

describe('matchPreset ① 穷举精确', () => {
  it('metric_id 精确命中（=== 大小写敏感），entry 最多命中一次', () => {
    const r = matchPreset(preset([entry(['fps'])]), 'perf', baseTree);
    expect(r.selected).toEqual(['f1:perf:fps']);
    expect(r.hits).toEqual([
      { groupId: null, compositeId: 'f1:perf:fps', matchedBy: 'exact', matchedName: 'fps' },
    ]);
    expect(r.unmatched).toEqual([]);
  });

  it('metric_id 路径大小写敏感：候选 "FPS" 不命中 metric_id "fps"', () => {
    const treeOnlyId = tree([
      { fileId: 'f1', plugins: [{ pluginId: 'perf', metrics: [{ id: 'fps', name: 'Frames Per Second' }] }] },
    ]);
    const r = matchPreset(preset([entry(['FPS'])]), 'perf', treeOnlyId);
    expect(r.selected).toEqual([]);
    expect(r.hits).toEqual([]);
    expect(r.unmatched).toEqual([{ want: undefined, groupId: null, names: ['FPS'] }]);
  });

  it('name 大小写不敏感命中：候选 "FPS" 命中 name "fps"', () => {
    const r = matchPreset(preset([entry(['FPS'])]), 'perf', baseTree);
    expect(r.selected).toEqual(['f1:perf:fps']);
    expect(r.hits).toEqual([
      { groupId: null, compositeId: 'f1:perf:fps', matchedBy: 'exact', matchedName: 'FPS' },
    ]);
  });

  it('多文件多插件域内只选目标插件（fps 只命中 perf，不碰 gpu 的 fps）', () => {
    const r = matchPreset(preset([entry(['fps']), entry(['cpu_temp'])]), 'perf', baseTree);
    expect(r.selected).toEqual(['f1:perf:fps', 'f2:perf:cpu_temp']);
    expect(r.hits.map((h) => h.compositeId)).toEqual(['f1:perf:fps', 'f2:perf:cpu_temp']);
    expect(r.unmatched).toEqual([]);
  });

  it('命中优先级：metric_id 匹配全局优先于 name 匹配', () => {
    const priorityTree = tree([
      {
        fileId: 'f1',
        plugins: [
          {
            pluginId: 'perf',
            metrics: [
              { id: 'xyz', name: 'fps' },
              { id: 'fps', name: 'FPS' },
            ],
          },
        ],
      },
    ]);
    const r = matchPreset(preset([entry(['fps'])]), 'perf', priorityTree);
    expect(r.selected).toEqual(['f1:perf:fps']);
  });

  it('顶层条目 groupId = null，分组条目 groupId = 分组 id', () => {
    const r = matchPreset(
      preset(
        [entry(['fps'])],
        [group('cpu', [entry(['cpu_usage']), entry(['cpu_temp'])])],
      ),
      'perf',
      baseTree,
    );
    expect(r.hits.map((h) => [h.compositeId, h.groupId])).toEqual([
      ['f1:perf:fps', null],
      ['f1:perf:cpu_usage', 'cpu'],
      ['f2:perf:cpu_temp', 'cpu'],
    ]);
    expect(r.unmatched).toEqual([]);
  });

  it('want 去重：同 want 两条目仅首个命中生效，后续条目整体跳过', () => {
    const r = matchPreset(
      preset([entry(['fps'], 'fps'), entry(['frame_time'], 'fps')]),
      'perf',
      baseTree,
    );
    expect(r.selected).toEqual(['f1:perf:fps']);
    expect(r.hits.map((h) => [h.compositeId, h.want])).toEqual([['f1:perf:fps', 'fps']]);
    expect(r.unmatched).toEqual([]);
  });

  it('want 去重跨层级：顶层命中后，分组内同 want 条目整体跳过', () => {
    const r = matchPreset(
      preset(
        [entry(['fps'], 'fps')],
        [group('cpu', [entry(['cpu_usage'], 'fps'), entry(['cpu_temp'])])],
      ),
      'perf',
      baseTree,
    );
    expect(r.selected).toEqual(['f1:perf:fps', 'f2:perf:cpu_temp']);
    expect(r.hits.map((h) => h.compositeId)).toEqual(['f1:perf:fps', 'f2:perf:cpu_temp']);
    expect(r.unmatched).toEqual([]);
  });

  it('want 未命中在先：后续同 want 条目仍可命中（wantSeen 以命中为准）', () => {
    const r = matchPreset(
      preset([entry(['no_such'], 'fps'), entry(['fps'], 'fps')]),
      'perf',
      baseTree,
    );
    expect(r.selected).toEqual(['f1:perf:fps']);
    expect(r.hits.map((h) => h.compositeId)).toEqual(['f1:perf:fps']);
    expect(r.unmatched).toEqual([{ want: 'fps', groupId: null, names: ['no_such'] }]);
  });

  it('零命中条目进 unmatched（want/names 原文），与命中条目并存', () => {
    const r = matchPreset(
      preset(
        [entry(['no_such_metric', 'Another Name'], 'slot'), entry(['cpu_usage'])],
        [group('cpu', [entry(['fps'])])],
      ),
      'perf',
      baseTree,
    );
    expect(r.selected).toEqual(['f1:perf:cpu_usage', 'f1:perf:fps']);
    expect(r.hits.map((h) => h.compositeId)).toEqual(['f1:perf:cpu_usage', 'f1:perf:fps']);
    expect(r.unmatched).toEqual([
      { want: 'slot', groupId: null, names: ['no_such_metric', 'Another Name'] },
    ]);
  });

  it('同 metric 被多条目命中：selected 去重保序，hits 逐条记录', () => {
    const r = matchPreset(preset([entry(['fps']), entry(['fps'])]), 'perf', baseTree);
    expect(r.selected).toEqual(['f1:perf:fps']);
    expect(r.hits).toHaveLength(2);
  });
});

describe('matchPreset ② keywords 模糊兜底', () => {
  it('穷举零命中 + keywords 子串命中（大小写不敏感）→ 全选 fuzzy', () => {
    const r = matchPreset(preset([entry(['no_such_metric'])], [], ['FPS', 'TIME']), 'perf', baseTree);
    expect(r.selected).toEqual(['f1:perf:fps', 'f2:perf:fps', 'f1:perf:frame_time']);
    expect(r.hits).toEqual([
      { compositeId: 'f1:perf:fps', groupId: null, matchedBy: 'fuzzy', matchedName: 'FPS' },
      { compositeId: 'f2:perf:fps', groupId: null, matchedBy: 'fuzzy', matchedName: 'FPS' },
      { compositeId: 'f1:perf:frame_time', groupId: null, matchedBy: 'fuzzy', matchedName: 'TIME' },
    ]);
    expect(r.unmatched).toEqual([{ want: undefined, groupId: null, names: ['no_such_metric'] }]);
  });

  it('模糊命中去重：一条 metric 只计一次（多 keyword 命中同一 metric）', () => {
    const r = matchPreset(preset([entry(['no_such_metric'])], [], ['fps', 'FPS']), 'perf', baseTree);
    expect(r.selected).toEqual(['f1:perf:fps', 'f2:perf:fps']);
    expect(r.hits.map((h) => [h.compositeId, h.matchedName])).toEqual([
      ['f1:perf:fps', 'fps'],
      ['f2:perf:fps', 'fps'],
    ]);
  });

  it('模糊可命中 name（大小写不敏感子串）：keyword "cpu" 命中 "CPU Usage"/"CPU Temperature"', () => {
    const r = matchPreset(preset([entry(['no_such_metric'])], [], ['cpu']), 'perf', baseTree);
    expect(r.selected).toEqual(['f1:perf:cpu_usage', 'f2:perf:cpu_temp']);
    expect(r.hits.every((h) => h.matchedBy === 'fuzzy')).toBe(true);
  });

  it('穷举有命中 → keywords 不启用', () => {
    const r = matchPreset(preset([entry(['fps'])], [], ['frame']), 'perf', baseTree);
    expect(r.selected).toEqual(['f1:perf:fps']);
    expect(r.hits.every((h) => h.matchedBy === 'exact')).toBe(true);
  });

  it('keywords 全部不命中 → 零命中结果（selected=[]、unmatched 清单）', () => {
    const r = matchPreset(preset([entry(['no_such_metric'])], [], ['nope']), 'perf', baseTree);
    expect(r.selected).toEqual([]);
    expect(r.hits).toEqual([]);
    expect(r.unmatched).toEqual([{ want: undefined, groupId: null, names: ['no_such_metric'] }]);
  });

  it('空/纯空白 keyword 跳过（防 includes 恒真全选）："" 与 "cpu" 混合只按 "cpu" 命中', () => {
    const r = matchPreset(preset([entry(['no_such_metric'])], [], ['', 'cpu']), 'perf', baseTree);
    expect(r.selected).toEqual(['f1:perf:cpu_usage', 'f2:perf:cpu_temp']);
    expect(r.hits.every((h) => h.matchedBy === 'fuzzy')).toBe(true);
  });

  it('keywords 全为空白 → 模糊兜底零命中、selected=[]（不触发全选）', () => {
    const r = matchPreset(preset([entry(['no_such_metric'])], [], ['  ', '\t\n']), 'perf', baseTree);
    expect(r.selected).toEqual([]);
    expect(r.hits).toEqual([]);
    expect(r.unmatched).toEqual([{ want: undefined, groupId: null, names: ['no_such_metric'] }]);
  });

  it('空预设 → 空结果', () => {
    const r = matchPreset(preset(), 'perf', baseTree);
    expect(r).toEqual({ selected: [], hits: [], unmatched: [] });
  });

  it('插件不在树中 → 零命中，unmatched 返回全部条目（按原文顺序）', () => {
    const r = matchPreset(
      preset(
        [entry(['fps'])],
        [group('cpu', [entry(['cpu_usage'])])],
        ['fps'],
      ),
      'ghost',
      baseTree,
    );
    expect(r.selected).toEqual([]);
    expect(r.hits).toEqual([]);
    expect(r.unmatched).toEqual([
      { want: undefined, groupId: null, names: ['fps'] },
      { want: undefined, groupId: 'cpu', names: ['cpu_usage'] },
    ]);
  });
});

describe('matchUserPreset', () => {
  it('多插件逐文件解析：同 metric 多文件全选，hits 带 plugin_id 分组', () => {
    const r = matchUserPreset(
      userPreset({ perf: ['fps'], gpu: ['gpu_usage'] }),
      baseTree,
    );
    expect(r.selected).toEqual(['f1:perf:fps', 'f2:perf:fps', 'f1:gpu:gpu_usage']);
    expect(r.hits).toEqual([
      { groupId: 'perf', compositeId: 'f1:perf:fps', matchedBy: 'exact', matchedName: 'fps' },
      { groupId: 'perf', compositeId: 'f2:perf:fps', matchedBy: 'exact', matchedName: 'fps' },
      { groupId: 'gpu', compositeId: 'f1:gpu:gpu_usage', matchedBy: 'exact', matchedName: 'gpu_usage' },
    ]);
    expect(r.unmatched).toEqual([]);
  });

  it('单插件部分命中：命中部分全选，未命中 metric_id 逐条进 unmatched', () => {
    const r = matchUserPreset(userPreset({ perf: ['fps', 'no_such'] }), baseTree);
    expect(r.selected).toEqual(['f1:perf:fps', 'f2:perf:fps']);
    expect(r.hits).toHaveLength(2);
    expect(r.unmatched).toEqual([{ groupId: 'perf', names: ['no_such'] }]);
  });

  it('键内部分命中（[a,b,c] 仅命中 a）：selected 含 a、unmatched 一条 names=[b,c]', () => {
    const r = matchUserPreset(userPreset({ perf: ['fps', 'no_such_1', 'no_such_2'] }), baseTree);
    expect(r.selected).toEqual(['f1:perf:fps', 'f2:perf:fps']);
    expect(r.hits.every((h) => h.compositeId === 'f1:perf:fps' || h.compositeId === 'f2:perf:fps')).toBe(true);
    expect(r.unmatched).toEqual([{ groupId: 'perf', names: ['no_such_1', 'no_such_2'] }]);
  });

  it('键内全命中 → 无 unmatched', () => {
    const r = matchUserPreset(userPreset({ perf: ['fps', 'frame_time'] }), baseTree);
    expect(r.selected).toEqual(['f1:perf:fps', 'f2:perf:fps', 'f1:perf:frame_time']);
    expect(r.unmatched).toEqual([]);
  });

  it('键内全零命中 → 一条 unmatched 含全部 metric_id', () => {
    const r = matchUserPreset(userPreset({ perf: ['nope1', 'nope2'] }), baseTree);
    expect(r.selected).toEqual([]);
    expect(r.hits).toEqual([]);
    expect(r.unmatched).toEqual([{ groupId: 'perf', names: ['nope1', 'nope2'] }]);
  });

  it('entries 缺失/非对象（损坏数据防御）→ 空结果，不抛 TypeError', () => {
    const noEntries = { id: 'up1', name: { zh: '用户预设', en: 'User Preset' } } as unknown as UserPreset;
    expect(matchUserPreset(noEntries, baseTree)).toEqual({ selected: [], hits: [], unmatched: [] });
    const arrayEntries = { id: 'up2', name: { zh: 'a', en: 'b' }, entries: ['fps'] } as unknown as UserPreset;
    expect(matchUserPreset(arrayEntries, baseTree)).toEqual({ selected: [], hits: [], unmatched: [] });
  });

  it('全零命中：selected=[]，每键一条 unmatched（groupId=plugin_id、names=该键全部 metric）', () => {
    const r = matchUserPreset(userPreset({ perf: ['nope1'], gpu: ['nope2'] }), baseTree);
    expect(r.selected).toEqual([]);
    expect(r.hits).toEqual([]);
    expect(r.unmatched).toEqual([
      { groupId: 'perf', names: ['nope1'] },
      { groupId: 'gpu', names: ['nope2'] },
    ]);
  });

  it('插件不在树中 → 该键零命中进 unmatched', () => {
    const r = matchUserPreset(userPreset({ ghost: ['x'] }), baseTree);
    expect(r.selected).toEqual([]);
    expect(r.unmatched).toEqual([{ groupId: 'ghost', names: ['x'] }]);
  });

  it('键内重复 metric：selected 去重保序', () => {
    const r = matchUserPreset(userPreset({ perf: ['fps', 'fps', 'frame_time'] }), baseTree);
    expect(r.selected).toEqual(['f1:perf:fps', 'f2:perf:fps', 'f1:perf:frame_time']);
    expect(r.unmatched).toEqual([]);
  });

  it('空 entries → 空结果', () => {
    const r = matchUserPreset(userPreset({}), baseTree);
    expect(r).toEqual({ selected: [], hits: [], unmatched: [] });
  });
});

describe('deriveUserPresetEntries', () => {
  it('正常聚合：按输入顺序分组聚合 metric_id', () => {
    expect(
      deriveUserPresetEntries(['f1:perf:fps', 'f1:perf:frame_time', 'f1:gpu:gpu_usage']),
    ).toEqual({ perf: ['fps', 'frame_time'], gpu: ['gpu_usage'] });
  });

  it('跨文件同 plugin 去重保序（保留首次出现位置）', () => {
    expect(
      deriveUserPresetEntries(['f1:perf:fps', 'f2:perf:fps', 'f1:perf:frame_time', 'f2:perf:fps']),
    ).toEqual({ perf: ['fps', 'frame_time'] });
  });

  it('畸形复合 id 忽略（长度 ≠ 3：缺段/多段/空串）', () => {
    expect(
      deriveUserPresetEntries(['f1:perf', 'f1', '', 'f1:perf:fps:extra', 'f1:perf:fps']),
    ).toEqual({ perf: ['fps'] });
  });

  it('空输入 → 空 entries', () => {
    expect(deriveUserPresetEntries([])).toEqual({});
  });
});
