import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SessionProvider } from '../state/session';
import TopBar from './TopBar';

/** Mocked Tauri bridge for the real-mode suite (invoke / event layer / dialog plugin). */
const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  open: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => undefined),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: tauri.open, save: vi.fn() }));

function renderTopBar(route = '/') {
  const onNavigate = vi.fn();
  const utils = render(
    <SessionProvider>
      <TopBar route={route} onNavigate={onNavigate} />
    </SessionProvider>,
  );
  return { ...utils, onNavigate };
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

  it('routes through nav links', async () => {
    const { onNavigate } = renderTopBar('/');
    fireEvent.click(screen.getByRole('button', { name: 'Plugins' }));
    expect(onNavigate).toHaveBeenCalledWith('/plugins');
  });
});

/** Real (production) mode: the ipc singleton must be rebuilt with VITE_AB_IPC=real. */
describe('TopBar real mode: 打开会话入口（契约 C3.1）', () => {
  beforeEach(() => {
    vi.stubEnv('VITE_AB_IPC', 'real');
    vi.stubEnv('MODE', 'production');
    tauri.invoke.mockReset();
    tauri.open.mockReset();
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
  });

  async function renderRealTopBar() {
    vi.resetModules();
    const [tl, { SessionProvider: RealSessionProvider }, { default: RealTopBar }] = await Promise.all([
      import('@testing-library/react'),
      import('../state/session'),
      import('./TopBar'),
    ]);
    const view = tl.render(
      <RealSessionProvider>
        <RealTopBar route="/" onNavigate={vi.fn()} />
      </RealSessionProvider>,
    );
    return { tl, view };
  }

  it('opens the session picker (absession filter, single) and loads the picked session', async () => {
    tauri.invoke.mockImplementation((cmd: string, args: { path?: string }) => {
      if (cmd === 'load_session') {
        return Promise.resolve({
          session: { path: args.path, saved_at_ms: 1, file_count: 0, selected_metric_count: 0 },
          loaded_file_ids: [],
          missing: [],
        });
      }
      if (cmd === 'list_plugins') return Promise.resolve([]);
      return Promise.resolve(null);
    });
    tauri.open.mockResolvedValue('C:\\sessions\\s.absession');
    const { tl, view } = await renderRealTopBar();
    try {
      const btn = view.container.querySelector('[data-testid="open-session-btn"]') as HTMLButtonElement;
      expect(btn).toBeTruthy();
      tl.fireEvent.click(btn);
      await tl.waitFor(() =>
        expect(tauri.open).toHaveBeenCalledWith({
          multiple: false,
          filters: [{ name: 'AnalysisBuddy Session', extensions: ['absession'] }],
          title: 'Open AnalysisBuddy Session',
        }),
      );
      await tl.waitFor(() =>
        expect(tauri.invoke).toHaveBeenCalledWith('load_session', { path: 'C:\\sessions\\s.absession' }),
      );
    } finally {
      view.unmount();
    }
  });

  it('does nothing when the picker is cancelled', async () => {
    tauri.invoke.mockResolvedValue([]);
    tauri.open.mockResolvedValue(null);
    const { tl, view } = await renderRealTopBar();
    try {
      const btn = view.container.querySelector('[data-testid="open-session-btn"]') as HTMLButtonElement;
      tl.fireEvent.click(btn);
      await tl.waitFor(() => expect(tauri.open).toHaveBeenCalled());
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(tauri.invoke).not.toHaveBeenCalledWith('load_session', expect.anything());
    } finally {
      view.unmount();
    }
  });
});
