import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SessionProvider } from '../state/session';
import TopBar from './TopBar';

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
