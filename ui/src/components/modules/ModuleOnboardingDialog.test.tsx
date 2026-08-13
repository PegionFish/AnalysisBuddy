/** ui/src/components/modules/ModuleOnboardingDialog.test.tsx — 首启模块引导（P1-01 建议 1-2）：
 *  首次（localStorage 'ab.module.onboarded' 缺失）显示一次；稍后再说/× 关闭后置位不再弹出；
 *  已安装模块列表来自 session state.plugins；推荐模块静态展示「从本地 ZIP 安装」；
 *  添加模块走 pickPluginZip + installPluginZip。 */

import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as realIpc from '../../ipc/real';
import { ipc } from '../../ipc/ipc';
import type { PluginInfo } from '../../ipc/types';
import { SessionProvider, useSession, type SessionAction, type SessionState } from '../../state/session';
import ModuleOnboardingDialog, { ONBOARDED_KEY } from './ModuleOnboardingDialog';

interface ProbeApi {
  state: SessionState | null;
  dispatch: React.Dispatch<SessionAction> | null;
}

function StateProbe({ api }: { api: ProbeApi }) {
  const { state, dispatch } = useSession();
  api.state = state;
  api.dispatch = dispatch;
  return null;
}

const PLUGINS: PluginInfo[] = [
  {
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
  },
  {
    id: 'demo-tool',
    display_name: 'Demo Tool',
    version: '0.4.2',
    state: 'ready',
    loaded_file_ids: [],
    capabilities: { annotate: true, subscribe: true, binary_sidecar: false },
    last_error: null,
    source: 'portable',
    builtin: false,
    disabled: false,
  },
];

function renderDialog(api?: ProbeApi) {
  return render(
    <SessionProvider>
      {api ? <StateProbe api={api} /> : null}
      <ModuleOnboardingDialog />
    </SessionProvider>,
  );
}

describe('ModuleOnboardingDialog 首启引导（P1-01）', () => {
  beforeEach(() => {
    localStorage.clear();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('首次启动（无本地偏好）显示弹窗：说明文案 + 添加模块/稍后再说/关闭', () => {
    renderDialog();
    expect(screen.getByTestId('module-onboarding')).toBeInTheDocument();
    expect(screen.getByText(/分析能力由模块提供/)).toBeInTheDocument();
    expect(screen.getByTestId('onboarding-add-btn')).toBeInTheDocument();
    expect(screen.getByTestId('onboarding-later-btn')).toBeInTheDocument();
    expect(screen.getByTestId('onboarding-close-btn')).toBeInTheDocument();
  });

  it('列出已安装模块与静态推荐模块（标注从本地 ZIP 安装）', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderDialog(api);
    act(() => {
      api.dispatch!({ type: 'plugins/set', plugins: PLUGINS });
    });

    expect(screen.getByText('Builtin CSV')).toBeInTheDocument();
    expect(screen.getByText('demo-tool')).toBeInTheDocument();
    expect(screen.getAllByTestId('installed-module')).toHaveLength(2);

    // 静态推荐：hwinfo-log / batteryinfoview，均标注「从本地 ZIP 安装」。
    expect(screen.getByText('HWiNFO 日志解析模块')).toBeInTheDocument();
    expect(screen.getByText('BatteryInfoView 解析模块')).toBeInTheDocument();
    expect(screen.getAllByText('从本地 ZIP 安装')).toHaveLength(2);
  });

  it('「稍后再说」写入本地偏好，重新挂载不再弹出', () => {
    const { unmount } = renderDialog();
    fireEvent.click(screen.getByTestId('onboarding-later-btn'));
    expect(screen.queryByTestId('module-onboarding')).not.toBeInTheDocument();
    expect(localStorage.getItem(ONBOARDED_KEY)).toBe('1');

    unmount();
    renderDialog();
    expect(screen.queryByTestId('module-onboarding')).not.toBeInTheDocument();
  });

  it('× 关闭同样置位，不再弹出', () => {
    const { unmount } = renderDialog();
    fireEvent.click(screen.getByTestId('onboarding-close-btn'));
    expect(screen.queryByTestId('module-onboarding')).not.toBeInTheDocument();
    expect(localStorage.getItem(ONBOARDED_KEY)).toBe('1');

    unmount();
    renderDialog();
    expect(screen.queryByTestId('module-onboarding')).not.toBeInTheDocument();
  });

  it('「添加模块」：pickPluginZip + installPluginZip，成功后已安装列表更新并提示', async () => {
    const installSpy = vi.spyOn(ipc, 'install_plugin_zip');
    vi.spyOn(realIpc, 'pickPluginZip').mockResolvedValue('C:\\zips\\fixture.zip');
    const api: ProbeApi = { state: null, dispatch: null };
    renderDialog(api);
    act(() => {
      api.dispatch!({ type: 'plugins/set', plugins: PLUGINS });
    });

    fireEvent.click(screen.getByTestId('onboarding-add-btn'));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(installSpy).toHaveBeenCalledWith({ path: 'C:\\zips\\fixture.zip', overwrite: false });
    expect(screen.getByTestId('onboarding-install-success')).toBeInTheDocument();
    expect(screen.getByText('fixture-csv')).toBeInTheDocument();
  });

  it('取消 ZIP 选择 → 不触发安装，弹窗保持打开', async () => {
    const installSpy = vi.spyOn(ipc, 'install_plugin_zip');
    vi.spyOn(realIpc, 'pickPluginZip').mockResolvedValue(null);
    renderDialog();

    fireEvent.click(screen.getByTestId('onboarding-add-btn'));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(installSpy).not.toHaveBeenCalled();
    expect(screen.getByTestId('module-onboarding')).toBeInTheDocument();
  });
});
