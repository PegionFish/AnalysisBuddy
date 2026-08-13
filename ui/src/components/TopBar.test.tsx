import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SessionProvider, useSession, type SessionAction, type SessionState } from '../state/session';
import TopBar from './TopBar';

/** P7 real-mode tests: mocked Tauri bridge (same harness as real-import-flow.test.tsx).
 *  Mock-mode tests never reach these modules; the mocks only matter for the real-mode
 *  describe below (mock mode is forced by vitest config VITE_AB_IPC=mock). */
const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  open: vi.fn(),
  save: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (channel: string, handler: (event: { payload: unknown }) => void) => {
    const prev = tauri.listeners.get(channel);
    tauri.listeners.set(channel, handler);
    return () => {
      if (tauri.listeners.get(channel) === handler) {
        if (prev) tauri.listeners.set(channel, prev);
        else tauri.listeners.delete(channel);
      }
    };
  }),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: tauri.open, save: tauri.save }));

function renderTopBar(route = '/') {
  const onNavigate = vi.fn();
  const utils = render(
    <SessionProvider>
      <TopBar route={route} onNavigate={onNavigate} />
    </SessionProvider>,
  );
  return { ...utils, onNavigate };
}

/** P10 probes: expose session state to observe that New Session resets only when confirmed. */
function FilesCountProbe() {
  const { state } = useSession();
  return <span data-testid="file-count">{state.files.length}</span>;
}

function SeedButton() {
  const { actions } = useSession();
  return (
    <button type="button" onClick={() => void actions.importFiles(['C:\\data\\seed.csv'])}>
      Seed Files
    </button>
  );
}

function renderTopBarForConfirm() {
  return render(
    <SessionProvider>
      <TopBar route="/" onNavigate={vi.fn()} />
      <SeedButton />
      <FilesCountProbe />
    </SessionProvider>,
  );
}

/** P1-03 probe: expose dispatch to seed missing/reopen_failed entries directly. */
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

function renderTopBarWithProbe(api: ProbeApi, route = '/') {
  return render(
    <SessionProvider>
      <StateProbe api={api} />
      <TopBar route={route} onNavigate={vi.fn()} />
    </SessionProvider>,
  );
}

async function advance(ms: number): Promise<void> {
  const step = 250;
  for (let t = 0; t < ms; t += step) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(Math.min(step, ms - t));
    });
  }
}

describe('TopBar (ipc-ui.md §4.1)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    document.documentElement.dataset.theme = '';
  });

  it('switches theme: dataset + localStorage persistence', async () => {
    renderTopBar();
    expect(document.documentElement.dataset.theme).toBe('light');

    fireEvent.click(screen.getByRole('button', { name: 'Theme' }));
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(localStorage.getItem('ab.theme')).toBe('dark');
  });

  it('switches language and re-renders UI text', async () => {
    renderTopBar();
    expect(screen.getByRole('button', { name: 'New Session' })).toBeInTheDocument();

    fireEvent.change(screen.getByRole('combobox', { name: 'Language' }), { target: { value: 'zh' } });
    await advance(100);

    expect(screen.getByRole('button', { name: '新建会话' })).toBeInTheDocument();
    expect(localStorage.getItem('ab.lang')).toBe('zh');
  });

  it('shows a missing-files badge after opening a session with missing entries', async () => {
    localStorage.setItem(
      'ab.mock.session',
      JSON.stringify({ path: 'C:\\sessions\\s.absession', saved_at_ms: 1, file_count: 1, selected_metric_count: 0, files: [{ file_id: 'f1', path: 'gone.csv' }] }),
    );
    renderTopBar();
    expect(screen.queryByTestId('missing-badge')).not.toBeInTheDocument();

    fireEvent.change(screen.getByRole('textbox', { name: 'Session file path' }), {
      target: { value: 'C:\\sessions\\missing.absession' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Open Session' }));
    await advance(500);

    expect(screen.getByTestId('missing-badge')).toHaveTextContent('1 missing file(s)');
  });

  it('shows a reopen-failed badge after opening a session with reopen failures', async () => {
    localStorage.setItem(
      'ab.mock.session',
      JSON.stringify({ path: 'C:\\sessions\\s.absession', saved_at_ms: 1, file_count: 1, selected_metric_count: 0, files: [{ file_id: 'f1', path: 'busy.csv' }] }),
    );
    renderTopBar();
    expect(screen.queryByTestId('reopen-failed-badge')).not.toBeInTheDocument();

    fireEvent.change(screen.getByRole('textbox', { name: 'Session file path' }), {
      target: { value: 'C:\\sessions\\reopen.absession' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Open Session' }));
    await advance(500);

    const badge = screen.getByTestId('reopen-failed-badge');
    expect(badge).toHaveTextContent('1 file(s) failed to reopen');
    expect(badge).toHaveAttribute('title', expect.stringContaining('busy.csv'));
  });

  it('save session persists to the mock localStorage slot', async () => {
    renderTopBar();
    fireEvent.click(screen.getByRole('button', { name: 'Save Session' }));
    await advance(500);
    expect(localStorage.getItem('ab.mock.session')).toContain('absession');
  });

  it('shows a success toast after saving and auto-dismisses it (P8)', async () => {
    renderTopBar();
    expect(screen.queryByTestId('save-notice')).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Save Session' }));
    await advance(500);

    const notice = screen.getByTestId('save-notice');
    expect(notice).toHaveAttribute('role', 'status');
    expect(notice).toHaveTextContent(/Saved to .*\.absession/);

    // 自动消退（SAVE_NOTICE_TTL_MS=4s）
    await advance(5000);
    expect(screen.queryByTestId('save-notice')).not.toBeInTheDocument();
  });

  it('Save As… also shows the success toast (P8)', async () => {
    renderTopBar();
    fireEvent.click(screen.getByRole('button', { name: 'Save As…' }));
    await advance(500);
    expect(screen.getByTestId('save-notice')).toHaveTextContent(/Saved to .*\.absession/);
  });

  it('dismisses the save toast manually (P8)', async () => {
    renderTopBar();
    fireEvent.click(screen.getByRole('button', { name: 'Save Session' }));
    await advance(500);
    expect(screen.getByTestId('save-notice')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
    expect(screen.queryByTestId('save-notice')).not.toBeInTheDocument();
  });

  it('new session asks for confirmation and resets only when confirmed (P10)', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);
    try {
      renderTopBarForConfirm();
      fireEvent.click(screen.getByRole('button', { name: 'Seed Files' }));
      await advance(300);
      expect(screen.getByTestId('file-count')).toHaveTextContent('1');

      // 取消：不重置
      fireEvent.click(screen.getByRole('button', { name: 'New Session' }));
      expect(confirmSpy).toHaveBeenCalled();
      expect(screen.getByTestId('file-count')).toHaveTextContent('1');

      // 确认：重置
      confirmSpy.mockReturnValue(true);
      fireEvent.click(screen.getByRole('button', { name: 'New Session' }));
      expect(screen.getByTestId('file-count')).toHaveTextContent('0');
    } finally {
      confirmSpy.mockRestore();
    }
  });

  it('declined confirmation keeps the session intact (P10)', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);
    try {
      renderTopBarForConfirm();
      fireEvent.click(screen.getByRole('button', { name: 'Seed Files' }));
      await advance(300);
      expect(screen.getByTestId('file-count')).toHaveTextContent('1');

      fireEvent.click(screen.getByRole('button', { name: 'New Session' }));
      expect(confirmSpy).toHaveBeenCalledWith(
        'Start a new session? All unsaved files, selected metrics, and cursor state will be lost.',
      );
      expect(screen.getByTestId('file-count')).toHaveTextContent('1');
    } finally {
      confirmSpy.mockRestore();
    }
  });

  it('routes through nav links', async () => {
    const { onNavigate } = renderTopBar('/');
    fireEvent.click(screen.getByRole('button', { name: 'Plugins' }));
    expect(onNavigate).toHaveBeenCalledWith('/plugins');
  });

  it('shows the recovery summary when missing/reopen-failed entries exist (P1-03)', async () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderTopBarWithProbe(api);
    expect(screen.queryByTestId('recovery-summary')).not.toBeInTheDocument();

    act(() => {
      api.dispatch!({ type: 'session/missing', entries: [{ path: 'C:\\data\\gone.csv', reason: 'not_found' }] });
      api.dispatch!({
        type: 'session/reopen_failed',
        entries: [{ path: 'C:\\data\\busy.csv', reason: 'reopen_failed' }],
      });
    });

    // 恢复摘要出现，且既有徽标保持不变（其他测试依赖它们）。
    expect(screen.getByTestId('recovery-summary')).toBeInTheDocument();
    expect(screen.getByTestId('missing-badge')).toBeInTheDocument();
    expect(screen.getByTestId('reopen-failed-badge')).toBeInTheDocument();
  });

  it('does not render the recovery summary without failures (P1-03)', async () => {
    renderTopBar();
    expect(screen.queryByTestId('recovery-summary')).not.toBeInTheDocument();
  });
});

describe('real mode: open session picker (P7)', () => {
  beforeEach(() => {
    vi.useRealTimers();
    vi.stubEnv('VITE_AB_IPC', 'real');
    vi.stubEnv('MODE', 'production');
    vi.restoreAllMocks();
    tauri.listeners.clear();
    tauri.invoke.mockReset();
    tauri.open.mockReset();
    tauri.save.mockReset();
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
  });

  async function renderRealTopBar() {
    vi.resetModules();
    const tl = await import('@testing-library/react');
    const { SessionProvider } = await import('../state/session');
    const { default: TopBar } = await import('./TopBar');
    const view = tl.render(
      <SessionProvider>
        <TopBar route="/" onNavigate={vi.fn()} />
      </SessionProvider>,
    );
    return { tl, view };
  }

  it('shows an Open Session… button that picks .absession via the native dialog and loads the session', async () => {
    tauri.invoke.mockImplementation(async (cmd: string) => {
      switch (cmd) {
        case 'load_session':
          return {
            session: { path: 'C:\\sessions\\real.absession', saved_at_ms: 1, file_count: 1, selected_metric_count: 0 },
            loaded_file_ids: ['f1'],
            missing: [],
            reopen_failed: [],
            time_ranges: [],
          };
        case 'get_metrics':
          return [];
        default:
          return {};
      }
    });
    tauri.open.mockResolvedValue('C:\\sessions\\real.absession');

    const { tl, view } = await renderRealTopBar();
    try {
      // 生产模式顶栏显示「打开会话…」按钮（原生选择器入口）
      const openBtn = view.container.querySelector<HTMLButtonElement>('[data-testid="open-session-pick"]');
      expect(openBtn).toBeTruthy();

      await tl.act(async () => {
        tl.fireEvent.click(openBtn!);
      });
      await tl.waitFor(() => expect(tauri.open).toHaveBeenCalledTimes(1));
      expect(tauri.open).toHaveBeenCalledWith(
        expect.objectContaining({
          multiple: false,
          filters: [{ name: 'AnalysisBuddy Session', extensions: ['absession'] }],
        }),
      );

      // 选中的路径 → load_session
      await tl.waitFor(() => {
        const calls = tauri.invoke.mock.calls.filter((c) => c[0] === 'load_session');
        expect(calls).toHaveLength(1);
        expect(calls[0][1]).toEqual({ path: 'C:\\sessions\\real.absession' });
      });
    } finally {
      view.unmount();
    }
  });

  it('cancelling the dialog is silent: no load_session call (P7)', async () => {
    tauri.open.mockResolvedValue(null);
    tauri.invoke.mockImplementation(async () => ({}));

    const { tl, view } = await renderRealTopBar();
    try {
      const openBtn = view.container.querySelector<HTMLButtonElement>('[data-testid="open-session-pick"]')!;
      await tl.act(async () => {
        tl.fireEvent.click(openBtn);
      });
      await tl.waitFor(() => expect(tauri.open).toHaveBeenCalledTimes(1));
      await new Promise((r) => setTimeout(r, 60));
      expect(tauri.invoke.mock.calls.filter((c) => c[0] === 'load_session')).toHaveLength(0);
    } finally {
      view.unmount();
    }
  });
});
