import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import { SessionProvider, useSession } from '../state/session';
import AppShell from './AppShell';
import FilePanel from './FilePanel';

/** Test harness: simulate a plugin crash via a health event (mock never crashes on its own). */
function CrashPlugin({ pluginId }: { pluginId: string }) {
  const { dispatch } = useSession();
  return (
    <button
      type="button"
      onClick={() =>
        dispatch({
          type: 'plugins/health',
          payload: { plugin_id: pluginId, state: 'crashed', prev_state: 'ready', detail: 'exit code 42' },
        })
      }
    >
      crash
    </button>
  );
}

function renderPage() {
  window.location.hash = '#/plugins';
  return render(
    <SessionProvider>
      <CrashPlugin pluginId="builtin-csv" />
      <FilePanel />
      <AppShell />
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

function rowById(id: string): HTMLElement {
  const row = screen.getAllByTestId('plugin-row').find((r) => r.getAttribute('data-plugin-id') === id);
  if (!row) throw new Error(`plugin row ${id} not found`);
  return row;
}

function badgeOf(id: string): HTMLElement {
  return within(rowById(id)).getByTestId('plugin-badge');
}

/** The innermost span carrying the line text; its parent .plugin-log__line holds the level class. */
function logLineFor(text: RegExp): HTMLElement {
  const line = screen.getByText(text).closest('.plugin-log__line');
  if (!line) throw new Error(`log line ${text} not found`);
  return line as HTMLElement;
}

describe('PluginManagerPage (ipc-ui.md §4.6)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    window.location.hash = '';
  });

  it('lists discovered plugins with ready badges, versions and capabilities', async () => {
    renderPage();
    await advance(500);

    expect(screen.getAllByTestId('plugin-row')).toHaveLength(2);
    expect(within(rowById('builtin-csv')).getByText('v1.0.0')).toBeInTheDocument();
    expect(badgeOf('builtin-csv')).toHaveTextContent('Ready');
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'ready');

    fireEvent.click(within(rowById('builtin-csv')).getByTestId('drawer-toggle'));
    const drawer = within(rowById('builtin-csv')).getByTestId('plugin-drawer');
    expect(drawer).toHaveTextContent('Annotate: Off');
    expect(drawer).toHaveTextContent('Subscribe: Off');
    expect(drawer).toHaveTextContent('Binary sidecar: Off');

    fireEvent.click(within(rowById('demo-tool')).getByTestId('drawer-toggle'));
    const drawer2 = within(rowById('demo-tool')).getByTestId('plugin-drawer');
    expect(drawer2).toHaveTextContent('Annotate: On');
    expect(drawer2).toHaveTextContent('Subscribe: On');
  });

  it('flips the badge live with ab://plugin-health through the import pipeline', async () => {
    renderPage();
    await advance(500);
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'ready');

    await importPath('C:\\data\\p.csv');
    await advance(200);
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'parsing');
    expect(badgeOf('builtin-csv')).toHaveTextContent('Parsing');
    expect(badgeOf('demo-tool')).toHaveAttribute('data-state', 'ready');

    await advance(10_000);
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'ready');
    expect(badgeOf('builtin-csv')).toHaveTextContent('Ready');
  });

  it('shows last_error on crashed and reload flips the badge back to ready', async () => {
    const spy = vi.spyOn(ipc, 'reload_plugin');
    renderPage();
    await advance(500);

    fireEvent.click(screen.getByRole('button', { name: 'crash' }));
    expect(badgeOf('builtin-csv')).toHaveTextContent('Crashed');
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'crashed');
    expect(within(rowById('builtin-csv')).getByTestId('plugin-last-error')).toHaveTextContent(
      'Last error: exit code 42',
    );

    fireEvent.click(within(rowById('builtin-csv')).getByTestId('reload-btn'));
    await advance(600);
    expect(spy).toHaveBeenCalledWith({ plugin_id: 'builtin-csv' });
    expect(badgeOf('builtin-csv')).toHaveTextContent('Ready');
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'ready');
    expect(within(rowById('builtin-csv')).queryByTestId('plugin-last-error')).not.toBeInTheDocument();
  });

  it('drawer backfills history via get_plugin_log on open and appends live events with level coloring', async () => {
    const spy = vi.spyOn(ipc, 'get_plugin_log');
    renderPage();
    await advance(500);

    await importPath('C:\\data\\logs.dat');
    await advance(500);

    fireEvent.click(within(rowById('demo-tool')).getByTestId('drawer-toggle'));
    await advance(500);
    expect(spy).toHaveBeenCalledWith({ plugin_id: 'demo-tool' });

    const scroller = screen.getByTestId('log-scroller');
    expect(scroller).toHaveTextContent('starting');
    expect(scroller).toHaveTextContent('handshake');
    expect(logLineFor(/locale config missing/).className).toContain('plugin-log__line--error');
    expect(scroller.querySelectorAll('.plugin-log__line').length).toBeGreaterThanOrEqual(3);

    await advance(10_000);
    expect(logLineFor(/slow batch/).className).toContain('plugin-log__line--warn');
    expect(scroller.querySelectorAll('.plugin-log__line').length).toBeGreaterThanOrEqual(5);
  });

  it('auto-scrolls to the bottom while following; up-scroll pauses and the follow button resumes', async () => {
    renderPage();
    await advance(500);
    await importPath('C:\\data\\logs.dat');
    await advance(500);

    fireEvent.click(within(rowById('demo-tool')).getByTestId('drawer-toggle'));
    await advance(500);
    const scroller = screen.getByTestId('log-scroller') as HTMLDivElement;
    expect(scroller.getAttribute('data-following')).toBe('true');

    scroller.scrollTop = 123;
    fireEvent.scroll(scroller);
    expect(scroller.getAttribute('data-following')).toBe('true');

    await advance(10_000);
    expect(screen.getByText(/slow batch/)).toBeInTheDocument();
    expect(scroller.scrollTop).toBe(0);

    scroller.scrollTop = 50;
    fireEvent.scroll(scroller);
    expect(scroller.getAttribute('data-following')).toBe('false');

    fireEvent.click(within(rowById('demo-tool')).getByTestId('reload-btn'));
    await advance(500);
    expect(screen.getByText(/reloaded/)).toBeInTheDocument();
    expect(scroller.scrollTop).toBe(50);

    fireEvent.click(screen.getByTestId('follow-btn'));
    expect(scroller.getAttribute('data-following')).toBe('true');
    expect(scroller.scrollTop).toBe(0);
  });

  it('drawer shows the loaded file ids of a ready plugin', async () => {
    renderPage();
    await advance(500);
    await importPath('C:\\data\\f.csv');
    await advance(10_000);

    fireEvent.click(within(rowById('builtin-csv')).getByTestId('drawer-toggle'));
    await advance(500);
    const files = within(rowById('builtin-csv')).getByTestId('loaded-files');
    expect(files).toHaveTextContent(/mock-f-/);
    expect(files).not.toHaveTextContent('—');
  });
});
