import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { EV_OS_DRAG_DROP, EV_OS_DRAG_ENTER, EV_OS_DRAG_LEAVE } from '../ipc/real';
import { ipc } from '../ipc/ipc';
import { SessionProvider } from '../state/session';
import FilePanel from './FilePanel';
import MetricTree from './MetricTree';

/** Mocked Tauri bridge for the real-mode suite below (invoke / event layer / dialog plugin). */
const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  open: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (channel: string, handler: (event: { payload: unknown }) => void) => {
    tauri.listeners.set(channel, handler);
    return () => {
      tauri.listeners.delete(channel);
    };
  }),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: tauri.open }));

function renderPanel() {
  return render(
    <SessionProvider>
      <FilePanel />
      <MetricTree />
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

async function importPath(path: string): Promise<void> {
  fireEvent.change(screen.getByTestId('path-input'), { target: { value: path } });
  fireEvent.click(screen.getByRole('button', { name: 'Import Files' }));
  await advance(500);
}

describe('FilePanel (ipc-ui.md §4.2)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('imports via the mock path input and shows parsing then ready with progress', async () => {
    renderPanel();

    await importPath('C:\\data\\run-1.csv');
    const entry = screen.getByTestId('file-entry');
    expect(within(entry).getByText('run-1.csv')).toBeInTheDocument();
    expect(within(entry).getByTestId('status-badge')).toHaveTextContent(/Parsing/);
    expect(within(entry).getByTestId('progress')).toBeInTheDocument();
    expect(within(entry).getAllByText(/builtin-csv/).length).toBeGreaterThan(0);

    await advance(10_000);
    expect(within(entry).getByTestId('status-badge')).toHaveTextContent('Ready');
  });

  it('shows error entries for fail-load paths and retry re-sends import_files([path])', async () => {
    const spy = vi.spyOn(ipc, 'import_files');
    renderPanel();

    await importPath('C:\\data\\fail-load.csv');
    const entry = screen.getByTestId('file-entry');
    expect(within(entry).getByTestId('entry-error')).toHaveTextContent('Failed to load file');

    spy.mockClear();
    fireEvent.click(within(entry).getByTestId('retry-btn'));
    await advance(500);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy.mock.calls[0][0].paths).toEqual(['C:\\data\\fail-load.csv']);
    const retried = screen.getByTestId('file-entry');
    expect(within(retried).getByTestId('entry-error')).toBeInTheDocument();
  });

  it('unloads a file entry via unload_file', async () => {
    const spy = vi.spyOn(ipc, 'unload_file');
    renderPanel();

    await importPath('C:\\data\\bye.csv');
    await advance(10_000);
    expect(screen.getByTestId('file-entry')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('unload-btn'));
    await advance(500);
    expect(spy).toHaveBeenCalledWith({ file_id: expect.any(String) });
    expect(screen.queryByTestId('file-entry')).not.toBeInTheDocument();
    expect(screen.getByText('No files yet')).toBeInTheDocument();
  });

  it('disable toggle greys the entry and unchecks its metrics', async () => {
    renderPanel();

    await importPath('C:\\data\\metric.csv');
    await advance(10_000);

    const metricBox = screen.getByRole('checkbox', { name: /metric_1/ });
    expect(metricBox).not.toBeDisabled();
    fireEvent.click(metricBox);
    await advance(1_000);
    expect(metricBox).toBeChecked();

    const entry = screen.getByTestId('file-entry');
    fireEvent.click(within(entry).getByRole('button', { name: 'Disable' }));
    expect(entry).toHaveClass('file-entry--disabled');
    expect(screen.getByRole('checkbox', { name: /metric_1/ })).toBeDisabled();
    expect(screen.getByRole('checkbox', { name: /metric_1/ })).not.toBeChecked();

    fireEvent.click(within(entry).getByRole('button', { name: 'Enable' }));
    expect(entry).not.toHaveClass('file-entry--disabled');
  });

  it('needs_user_choice shows candidate buttons and re-imports with overrides on pick', async () => {
    const spy = vi.spyOn(ipc, 'import_files');
    renderPanel();

    await importPath('C:\\data\\choice.dat');
    const entry = screen.getByTestId('file-entry');
    expect(within(entry).getByTestId('plugin-choice')).toBeInTheDocument();

    fireEvent.click(within(entry).getByRole('button', { name: /builtin-csv/ }));
    expect(spy).toHaveBeenCalledWith({
      paths: ['C:\\data\\choice.dat'],
      overrides: { 'C:\\data\\choice.dat': { plugin_id: 'builtin-csv' } },
    });
    await advance(500);
    expect(within(entry).getByTestId('status-badge')).toHaveTextContent(/Parsing/);
  });
});

/** Real (production) mode: the ipc singleton must be rebuilt with VITE_AB_IPC=real, so these
 *  tests reset the module registry and dynamically import a fresh component tree. */
describe('FilePanel real mode: OS drag&drop + file picker (task 13)', () => {
  beforeEach(() => {
    vi.stubEnv('VITE_AB_IPC', 'real');
    vi.stubEnv('MODE', 'production');
    tauri.listeners.clear();
    tauri.invoke.mockReset();
    tauri.open.mockReset();
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
  });

  async function renderRealPanel() {
    vi.resetModules();
    const [tl, { SessionProvider: RealSessionProvider }, { default: RealFilePanel }] = await Promise.all([
      import('@testing-library/react'),
      import('../state/session'),
      import('./FilePanel'),
    ]);
    const view = tl.render(
      <RealSessionProvider>
        <RealFilePanel />
      </RealSessionProvider>,
    );
    return { tl, view };
  }

  it('subscribes tauri://drag-drop (real mode only) and imports the dropped paths', async () => {
    tauri.invoke.mockResolvedValue([]);
    const { tl, view } = await renderRealPanel();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      tauri.invoke.mockClear();
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_DROP)?.({
          payload: { paths: ['C:\\real\\run-1.csv'], position: { x: 0, y: 0 } },
        });
      });
      await tl.waitFor(() =>
        expect(tauri.invoke).toHaveBeenCalledWith('import_files', {
          paths: ['C:\\real\\run-1.csv'],
          overrides: undefined,
        }),
      );
    } finally {
      view.unmount();
    }
  });

  it('drag-enter highlights the dropzone and drag-leave clears it', async () => {
    tauri.invoke.mockResolvedValue([]);
    const { tl, view } = await renderRealPanel();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_ENTER)).toBe(true));
      const zone = view.container.querySelector('[data-testid="dropzone"]') as HTMLElement;
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_ENTER)?.({ payload: {} });
      });
      await tl.waitFor(() => expect(zone.className).toContain('file-panel__dropzone--over'));
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_LEAVE)?.({ payload: {} });
      });
      await tl.waitFor(() => expect(zone.className).not.toContain('file-panel__dropzone--over'));
    } finally {
      view.unmount();
    }
  });

  it('the choose-files button opens the native dialog and imports picked paths', async () => {
    tauri.invoke.mockResolvedValue([]);
    tauri.open.mockResolvedValue(['C:\\picked\\a.csv', 'C:\\picked\\b.log']);
    const { tl, view } = await renderRealPanel();
    try {
      const btn = view.container.querySelector('[data-testid="pick-files-btn"]') as HTMLButtonElement;
      expect(btn).toBeTruthy();
      tl.fireEvent.click(btn);
      await tl.waitFor(() => expect(tauri.open).toHaveBeenCalledWith({ multiple: true }));
      await tl.waitFor(() =>
        expect(tauri.invoke).toHaveBeenCalledWith('import_files', {
          paths: ['C:\\picked\\a.csv', 'C:\\picked\\b.log'],
          overrides: undefined,
        }),
      );
    } finally {
      view.unmount();
    }
  });

  it('does nothing when the file dialog is cancelled', async () => {
    tauri.invoke.mockResolvedValue([]);
    tauri.open.mockResolvedValue(null);
    const { tl, view } = await renderRealPanel();
    try {
      const btn = view.container.querySelector('[data-testid="pick-files-btn"]') as HTMLButtonElement;
      tl.fireEvent.click(btn);
      await tl.waitFor(() => expect(tauri.open).toHaveBeenCalled());
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(tauri.invoke).not.toHaveBeenCalled();
    } finally {
      view.unmount();
    }
  });

  it('shows the import error banner when import_files rejects', async () => {
    tauri.invoke.mockRejectedValue({ code: 'internal', message: 'host unavailable' });
    const { tl, view } = await renderRealPanel();
    try {
      const { default: i18nFresh } = await import('../i18n');
      await tl.waitFor(() => expect(i18nFresh.isInitialized).toBe(true));
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_DROP)?.({
          payload: { paths: ['C:\\real\\broken.csv'], position: { x: 0, y: 0 } },
        });
      });
      await tl.waitFor(() => {
        const banner = view.container.querySelector('[data-testid="import-error"]');
        expect(banner?.textContent).toContain('Import failed');
        expect(banner?.textContent).toContain('host unavailable');
      });
    } finally {
      view.unmount();
    }
  });

  it('surfaces raw string rejections verbatim instead of swallowing them (task 15 defect 4)', async () => {
    // Tauri ACL rejects arrive as plain strings (e.g. "Command import_files not allowed by ACL").
    // Regression guard: the banner must show the original text, not "internal error".
    tauri.invoke.mockRejectedValue('Command import_files not allowed by ACL');
    const { tl, view } = await renderRealPanel();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_DROP)?.({
          payload: { paths: ['C:\\real\\acl.csv'], position: { x: 0, y: 0 } },
        });
      });
      await tl.waitFor(() => {
        const banner = view.container.querySelector('[data-testid="import-error"]');
        expect(banner?.textContent).toContain('Command import_files not allowed by ACL');
      });
    } finally {
      view.unmount();
    }
  });
});
