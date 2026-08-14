/** ui/src/components/PresetBar.test.tsx — Wave 4 C10 场景预设工具条组件测试。
 *  覆盖：渲染（插件预设/用户预设来源标记、空态）、应用（命中/零命中/部分命中
 *  附带 unmatched）、保存（空名校验/成功刷新列表/失败横幅）、删除（内联确认/
 *  取消/幂等/清选中）。全部经 mock 轨 ipc（createMockIpc，VITE_AB_IPC=mock）。 */
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import type { MetricNode, PluginInfo, PresetDef, UserPreset } from '../ipc/types';
import { SessionProvider, useSession, type SessionAction, type SessionState } from '../state/session';
import PresetBar from './PresetBar';
import TopBar from './TopBar';

/** mock 用户预设槽位（createMockIpc 的 PRESETS_KEY 约定）。 */
const PRESETS_KEY = 'ab.mock.presets';

/** 插件预设夹具（形状即 PresetDef 契约：entries 精确候选 + keywords 模糊兜底）。 */
const CSV_PRESET: PresetDef = {
  id: 'csv-metrics',
  name: { zh: 'CSV 指标预设', en: 'CSV Metrics' },
  entries: [
    { want: 'fps', names: ['fps', 'frame_rate'] },
    { names: ['timestamp', 'frame_ms'] },
  ],
  groups: [],
  keywords: ['csv', 'metrics', 'performance'],
};

const CSV_PLUGIN: PluginInfo = {
  id: 'builtin-csv',
  display_name: 'Builtin CSV',
  version: '1.0.0',
  state: 'ready',
  loaded_file_ids: [],
  capabilities: { annotate: false, subscribe: false, binary_sidecar: false },
  last_error: null,
  source: 'portable',
  builtin: true,
  disabled: false,
  presets: [CSV_PRESET],
};

/** 用户预设夹具（entries 按 plugin_id 分键）。 */
const MY_PRESET: UserPreset = {
  id: 'my-preset',
  name: { zh: '我的预设', en: 'My Preset' },
  entries: { 'builtin-csv': ['fps', 'timestamp'] },
};

/** 自造 file → plugin(builtin-csv) → metric 三棵树。 */
function csvTree(metricIds: string[]): MetricNode[] {
  return [
    {
      level: 'file',
      id: 'f1',
      file_id: 'f1',
      name: 'bench.csv',
      children: [
        {
          level: 'plugin',
          id: 'f1:builtin-csv',
          file_id: 'f1',
          plugin_id: 'builtin-csv',
          name: 'Builtin CSV',
          children: metricIds.map((mid) => ({
            level: 'metric',
            id: `f1:builtin-csv:${mid}`,
            file_id: 'f1',
            plugin_id: 'builtin-csv',
            metric_id: mid,
            name: mid,
          })),
        },
      ],
    },
  ];
}

interface ProbeApi {
  state: SessionState | null;
  dispatch: React.Dispatch<SessionAction> | null;
}

/** 状态探针：把 SessionState/dispatch 暴露给用例（MetricTree.test 同风格）。 */
function StateProbe({ api }: { api: ProbeApi }) {
  const { state, dispatch } = useSession();
  api.state = state;
  api.dispatch = dispatch;
  return null;
}

function renderBar(api: ProbeApi) {
  return render(
    <SessionProvider>
      <StateProbe api={api} />
      <PresetBar />
    </SessionProvider>,
  );
}

/** 通过 dispatch 注入插件列表（AppShell 挂载时的 list_plugins 同路径）。 */
function seedPlugins(api: ProbeApi, plugins: PluginInfo[]): void {
  act(() => {
    api.dispatch!({ type: 'plugins/set', plugins });
  });
}

/** 通过 dispatch 注入自造指标树。 */
function seedTree(api: ProbeApi, tree: MetricNode[]): void {
  act(() => {
    api.dispatch!({ type: 'metrics/set', tree });
  });
}

/** 通过 dispatch 预置选中指标（保存预设的反推输入）。 */
function seedSelection(api: ProbeApi, ids: string[]): void {
  act(() => {
    api.dispatch!({ type: 'metrics/toggle', ids, checked: true });
  });
}

/** 预置 mock 用户预设槽位（组件挂载/保存后经 list_user_presets 读取）。 */
function seedUserPresets(store: Record<string, UserPreset>): void {
  localStorage.setItem(PRESETS_KEY, JSON.stringify(store));
}

/** 推进假计时器，让 mock ipc 的 40–150ms 命令延迟落地（MetricTree.test 同风格）。 */
async function advance(ms: number): Promise<void> {
  const step = 250;
  for (let t = 0; t < ms; t += step) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(Math.min(step, ms - t));
    });
  }
}

const select = () => screen.getByRole('combobox', { name: 'Presets' });
const applyBtn = () => screen.getByRole('button', { name: 'Apply' });
const saveBtn = () => screen.getByRole('button', { name: /Save as preset/ });
const notice = () => screen.getByTestId('preset-notice');

describe('PresetBar (Wave 4 C10 场景预设工具条)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  describe('渲染：合并清单与来源标记', () => {
    it('插件预设与用户预设均出现在清单，来源标记正确', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      seedUserPresets({ 'my-preset': MY_PRESET });
      renderBar(api);
      seedPlugins(api, [CSV_PLUGIN]);
      await advance(500);

      // 插件预设：来源 = source_plugin（Plugin）+ 插件 display_name（Builtin CSV）
      expect(screen.getByRole('option', { name: 'Plugin · Builtin CSV · CSV Metrics' })).toBeInTheDocument();
      // 用户预设：来源 = source_user（Mine）
      expect(screen.getByRole('option', { name: 'Mine · My Preset' })).toBeInTheDocument();
      // 有预设时不再显示空态占位
      expect(screen.queryByText('No presets')).not.toBeInTheDocument();
    });

    it('无任何预设 → 空态占位，下拉与应用禁用', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      renderBar(api);
      await advance(500);

      expect(screen.getByRole('option', { name: 'No presets' })).toBeInTheDocument();
      expect(select()).toBeDisabled();
      expect(applyBtn()).toBeDisabled();
    });
  });

  describe('应用', () => {
    it('插件预设命中 → applyPreset 原子替换选择 + applied toast，选中项保持', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      seedUserPresets({});
      renderBar(api);
      seedPlugins(api, [CSV_PLUGIN]);
      seedTree(api, csvTree(['fps', 'timestamp']));
      await advance(500);

      fireEvent.change(select(), { target: { value: 'plugin:builtin-csv:csv-metrics' } });
      fireEvent.click(applyBtn());

      expect(api.state!.selectedMetrics).toEqual(new Set(['f1:builtin-csv:fps', 'f1:builtin-csv:timestamp']));
      expect(within(notice()).getByText('Applied preset CSV Metrics (2 metrics)')).toBeInTheDocument();
      // 应用后选中项保持
      expect(select()).toHaveValue('plugin:builtin-csv:csv-metrics');
    });

    it('零命中 → 不 dispatch（原选择原样保留）+ no_match toast 附 unmatched 数', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      seedUserPresets({});
      renderBar(api);
      seedPlugins(api, [CSV_PLUGIN]);
      seedTree(api, csvTree(['other']));
      seedSelection(api, ['f1:builtin-csv:other']);
      await advance(500);

      fireEvent.change(select(), { target: { value: 'plugin:builtin-csv:csv-metrics' } });
      fireEvent.click(applyBtn());

      // 若误 dispatch（空替换），预置选择会被清空——原样保留即证明未 dispatch。
      expect(api.state!.selectedMetrics).toEqual(new Set(['f1:builtin-csv:other']));
      expect(within(notice()).getByText('Preset "CSV Metrics" matched nothing; selection unchanged')).toBeInTheDocument();
      expect(within(notice()).getByText('2 entries unmatched')).toBeInTheDocument();
    });

    it('用户预设部分命中 → applied toast 附带未命中数', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      // unmatched 按 plugin_id 键统计（matchUserPreset 语义）：零命中的键进 unmatched。
      seedUserPresets({
        partial: {
          id: 'partial',
          name: { zh: '部分预设', en: 'Partial Preset' },
          entries: { 'builtin-csv': ['fps'], 'ghost-plugin': ['missing_metric'] },
        },
      });
      renderBar(api);
      seedTree(api, csvTree(['fps']));
      await advance(500);

      fireEvent.change(select(), { target: { value: 'user:partial' } });
      fireEvent.click(applyBtn());

      expect(api.state!.selectedMetrics).toEqual(new Set(['f1:builtin-csv:fps']));
      expect(within(notice()).getByText('Applied preset Partial Preset (1 metrics)')).toBeInTheDocument();
      expect(within(notice()).getByText('1 entries unmatched')).toBeInTheDocument();
    });

    it('用户预设零命中 → 不 dispatch + no_match toast', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      seedUserPresets({ 'my-preset': MY_PRESET });
      renderBar(api);
      seedTree(api, csvTree(['other']));
      seedSelection(api, ['f1:builtin-csv:other']);
      await advance(500);

      fireEvent.change(select(), { target: { value: 'user:my-preset' } });
      fireEvent.click(applyBtn());

      expect(api.state!.selectedMetrics).toEqual(new Set(['f1:builtin-csv:other']));
      expect(within(notice()).getByText('Preset "My Preset" matched nothing; selection unchanged')).toBeInTheDocument();
    });
  });

  describe('保存为预设', () => {
    it('无选中指标 → 保存按钮禁用 + no_selection 提示可见，无法保存', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      renderBar(api);
      await advance(500);

      fireEvent.click(saveBtn());
      const dialog = screen.getByTestId('preset-save-dialog');
      expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled();
      expect(within(dialog).getByTestId('preset-save-no-selection')).toHaveTextContent(
        'No metrics selected to save',
      );
    });

    it('有选中指标 → 保存按钮可用，无 no_selection 提示', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      renderBar(api);
      seedSelection(api, ['f1:builtin-csv:fps']);
      await advance(500);

      fireEvent.click(saveBtn());
      const dialog = screen.getByTestId('preset-save-dialog');
      expect(within(dialog).getByRole('button', { name: 'Save' })).toBeEnabled();
      expect(within(dialog).queryByTestId('preset-save-no-selection')).not.toBeInTheDocument();
    });

    it('命名对话框：空名 → name_required 提示，对话框保持打开', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      renderBar(api);
      seedSelection(api, ['f1:builtin-csv:fps']);
      await advance(500);

      fireEvent.click(saveBtn());
      const dialog = screen.getByTestId('preset-save-dialog');
      fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));

      expect(within(dialog).getByRole('alert')).toHaveTextContent('Name must not be empty');
      expect(screen.getByTestId('preset-save-dialog')).toBeInTheDocument();
    });

    it('保存成功：写入槽位、关闭对话框、重新拉取列表（新预设出现在清单）', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      seedUserPresets({});
      renderBar(api);
      seedSelection(api, ['f1:builtin-csv:fps']);
      await advance(500);

      fireEvent.click(saveBtn());
      const dialog = screen.getByTestId('preset-save-dialog');
      fireEvent.change(within(dialog).getByRole('textbox', { name: 'Preset name' }), {
        target: { value: 'fps monitor' },
      });
      fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
      await advance(500);

      expect(screen.queryByTestId('preset-save-dialog')).not.toBeInTheDocument();
      // 保存后重新 fetch 用户预设列表 → 新条目出现在清单
      expect(screen.getByRole('option', { name: 'Mine · fps monitor' })).toBeInTheDocument();
      const stored = JSON.parse(localStorage.getItem(PRESETS_KEY)!);
      expect(stored['fps-monitor']).toMatchObject({
        name: { zh: 'fps monitor', en: 'fps monitor' },
        entries: { 'builtin-csv': ['fps'] },
      });
    });

    it('保存失败（重名冲突）→ 会话层 saveError 横幅，清单不新增', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      seedUserPresets({
        'fps-monitor': { id: 'fps-monitor', name: { zh: 'fps monitor', en: 'fps monitor' }, entries: { 'builtin-csv': ['fps'] } },
      });
      render(
        <SessionProvider>
          <StateProbe api={api} />
          <TopBar route="/" onNavigate={vi.fn()} />
          <PresetBar />
        </SessionProvider>,
      );
      seedSelection(api, ['f1:builtin-csv:fps']);
      await advance(500);

      fireEvent.click(saveBtn());
      const dialog = screen.getByTestId('preset-save-dialog');
      fireEvent.change(within(dialog).getByRole('textbox', { name: 'Preset name' }), {
        target: { value: 'fps monitor' },
      });
      fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
      await advance(500);

      expect(screen.getByTestId('save-error')).toHaveTextContent(/Failed to save preset:/);
      // 重名冲突：清单仍只有原有 1 条，未新增重复项
      expect(screen.getAllByRole('option', { name: /fps monitor/ })).toHaveLength(1);
    });
  });

  describe('删除', () => {
    it('内联确认 → delete_user_preset → toast + 刷新列表 + 清选中', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      const deleteSpy = vi.spyOn(ipc, 'delete_user_preset');
      seedUserPresets({ 'my-preset': MY_PRESET });
      renderBar(api);
      await advance(500);

      fireEvent.change(select(), { target: { value: 'user:my-preset' } });
      fireEvent.click(screen.getByTestId('preset-delete'));

      const confirm = screen.getByTestId('preset-delete-confirm');
      expect(confirm).toHaveTextContent('Delete preset "My Preset"?');
      fireEvent.click(within(confirm).getByTestId('preset-delete-confirm-btn'));
      await advance(500);

      expect(deleteSpy).toHaveBeenCalledWith({ id: 'my-preset' });
      expect(within(notice()).getByText('Preset deleted')).toBeInTheDocument();
      // 刷新列表后条目移除；删除当前选中项后清选中
      expect(screen.queryByRole('option', { name: /My Preset/ })).not.toBeInTheDocument();
      expect(select()).toHaveValue('');
      expect(applyBtn()).toBeDisabled();
      expect(screen.queryByTestId('preset-delete')).not.toBeInTheDocument();
    });

    it('取消确认 → 不调用 ipc，预设保留', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      const deleteSpy = vi.spyOn(ipc, 'delete_user_preset');
      seedUserPresets({ 'my-preset': MY_PRESET });
      renderBar(api);
      await advance(500);

      fireEvent.change(select(), { target: { value: 'user:my-preset' } });
      fireEvent.click(screen.getByTestId('preset-delete'));
      fireEvent.click(screen.getByTestId('preset-delete-cancel-btn'));

      expect(deleteSpy).not.toHaveBeenCalled();
      expect(screen.queryByTestId('preset-delete-confirm')).not.toBeInTheDocument();
      expect(screen.getByRole('option', { name: /My Preset/ })).toBeInTheDocument();
      expect(screen.queryByTestId('preset-notice')).not.toBeInTheDocument();
    });

    it('幂等：预设已在他处删除时 mock 返回 Ok，仍刷新并 toast', async () => {
      const api: ProbeApi = { state: null, dispatch: null };
      seedUserPresets({ 'my-preset': MY_PRESET });
      renderBar(api);
      await advance(500);

      fireEvent.change(select(), { target: { value: 'user:my-preset' } });
      // 模拟他处（另一实例）已删除该预设：槽位为空
      localStorage.setItem(PRESETS_KEY, '{}');
      fireEvent.click(screen.getByTestId('preset-delete'));
      fireEvent.click(screen.getByTestId('preset-delete-confirm-btn'));
      await advance(500);

      expect(within(notice()).getByText('Preset deleted')).toBeInTheDocument();
      expect(screen.queryByRole('option', { name: /My Preset/ })).not.toBeInTheDocument();
      expect(select()).toHaveValue('');
    });
  });
});
