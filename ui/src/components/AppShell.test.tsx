import { act, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import { SessionProvider } from '../state/session';
import AppShell from './AppShell';

describe('AppShell (ipc-ui.md §4.1)', () => {
  afterEach(() => {
    window.location.hash = '';
    vi.unstubAllGlobals();
  });

  it('loads plugins on mount and renders the three-column workbench', async () => {
    const spy = vi.spyOn(ipc, 'list_plugins');
    render(
      <SessionProvider>
        <AppShell />
      </SessionProvider>,
    );

    expect(spy).toHaveBeenCalledTimes(1);
    expect(await screen.findByText('Files')).toBeInTheDocument();
    expect(screen.getByText('Metrics')).toBeInTheDocument();
    expect(screen.getByText('Start Analyzing')).toBeInTheDocument();
    expect(screen.getByText('Key Values')).toBeInTheDocument();
  });

  it('navigates to the plugins page via hash route', async () => {
    render(
      <SessionProvider>
        <AppShell />
      </SessionProvider>,
    );
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Plugins' }));
    expect(screen.getByTestId('plugins-page')).toBeInTheDocument();
    expect(window.location.hash).toBe('#/plugins');  });
});

describe('AppShell P2-04: collapsible/draggable sidebars + narrow-window key-values drawer', () => {
  afterEach(() => {
    window.location.hash = '';
    vi.unstubAllGlobals();
  });

  function renderShell() {
    return render(
      <SessionProvider>
        <AppShell />
      </SessionProvider>,
    );
  }

  /** matchMedia stub whose change listeners can be driven synchronously by the test. */
  function stubMatchMedia(initial: boolean) {
    let matches = initial;
    const listeners: Array<(e: { matches: boolean }) => void> = [];
    const mql = {
      get matches() {
        return matches;
      },
      media: '(max-width: 999px)',
      addEventListener: vi.fn((_type: string, cb: (e: { matches: boolean }) => void) => {
        listeners.push(cb);
      }),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(),
    };
    vi.stubGlobal('matchMedia', vi.fn(() => mql));
    return {
      setMatches(next: boolean) {
        matches = next;
        listeners.forEach((cb) => cb({ matches: next }));
      },
    };
  }

  function shellBody(): HTMLElement {
    return document.querySelector('.app-shell__body') as HTMLElement;
  }

  it('left sidebar collapses to a narrow rail via its button and persists to localStorage', async () => {
    const user = userEvent.setup();
    renderShell();
    const aside = screen.getByTestId('app-shell-left');
    expect(screen.getByTestId('collapse-left')).toHaveAccessibleName('Collapse left panel');
    expect(aside).not.toHaveClass('app-shell__left--collapsed');

    await user.click(screen.getByTestId('collapse-left'));
    expect(aside).toHaveClass('app-shell__left--collapsed');
    expect(localStorage.getItem('ab.layout.leftCollapsed')).toBe('1');
    expect(screen.getByTestId('collapse-left')).toHaveAccessibleName('Expand left panel');

    await user.click(screen.getByTestId('collapse-left'));
    expect(aside).not.toHaveClass('app-shell__left--collapsed');
    expect(localStorage.getItem('ab.layout.leftCollapsed')).toBe('0');
  });

  it('right sidebar collapses/expands independently and persists to localStorage', async () => {
    const user = userEvent.setup();
    renderShell();
    const aside = screen.getByTestId('app-shell-right');
    expect(screen.getByTestId('collapse-right')).toHaveAccessibleName('Collapse right panel');

    await user.click(screen.getByTestId('collapse-right'));
    expect(aside).toHaveClass('app-shell__right--collapsed');
    expect(localStorage.getItem('ab.layout.rightCollapsed')).toBe('1');
    expect(screen.getByTestId('collapse-right')).toHaveAccessibleName('Expand right panel');

    await user.click(screen.getByTestId('collapse-right'));
    expect(aside).not.toHaveClass('app-shell__right--collapsed');
    expect(localStorage.getItem('ab.layout.rightCollapsed')).toBe('0');
  });

  it('renders drag handles on both sidebars inner edges', () => {
    renderShell();
    expect(screen.getByTestId('resize-left')).toBeInTheDocument();
    expect(screen.getByTestId('resize-right')).toBeInTheDocument();
    expect(screen.getByTestId('resize-left')).toHaveAttribute('role', 'separator');
  });

  it('left handle drags resize the sidebar via --ab-left-w (clamped 200-480px) and persist', () => {
    renderShell();
    const body = shellBody();
    expect(body.style.getPropertyValue('--ab-left-w')).toBe('280px');

    fireEvent.pointerDown(screen.getByTestId('resize-left'), { clientX: 300 });
    expect(document.body.classList.contains('ab-resizing')).toBe(true);

    fireEvent.pointerMove(window, { clientX: 350 });
    expect(body.style.getPropertyValue('--ab-left-w')).toBe('350px');
    expect(localStorage.getItem('ab.layout.leftW')).toBe('350');

    fireEvent.pointerMove(window, { clientX: 30 });
    expect(body.style.getPropertyValue('--ab-left-w')).toBe('200px');

    fireEvent.pointerMove(window, { clientX: 9999 });
    expect(body.style.getPropertyValue('--ab-left-w')).toBe('480px');

    fireEvent.pointerUp(window);
    expect(document.body.classList.contains('ab-resizing')).toBe(false);
  });

  it('right handle drags resize mirrored (clamped 220-420px) and persist', () => {
    renderShell();
    const body = shellBody();
    expect(body.style.getPropertyValue('--ab-right-w')).toBe('320px');

    fireEvent.pointerDown(screen.getByTestId('resize-right'), { clientX: 700 });
    fireEvent.pointerMove(window, { clientX: 600 });
    expect(body.style.getPropertyValue('--ab-right-w')).toBe('420px');
    expect(localStorage.getItem('ab.layout.rightW')).toBe('420');

    fireEvent.pointerMove(window, { clientX: 900 });
    expect(body.style.getPropertyValue('--ab-right-w')).toBe('220px');

    fireEvent.pointerUp(window);
    expect(document.body.classList.contains('ab-resizing')).toBe(false);
  });

  it('restores persisted widths and tolerates corrupted values', () => {
    localStorage.setItem('ab.layout.leftW', '420');
    localStorage.setItem('ab.layout.rightW', 'garbage');
    localStorage.setItem('ab.layout.leftCollapsed', 'not-a-bool');
    renderShell();
    const body = shellBody();
    expect(body.style.getPropertyValue('--ab-left-w')).toBe('420px');
    expect(body.style.getPropertyValue('--ab-right-w')).toBe('320px');
    expect(screen.getByTestId('app-shell-left')).not.toHaveClass('app-shell__left--collapsed');
  });

  it('below 1000px the key-values panel moves into a floating drawer toggled by a button', async () => {
    stubMatchMedia(true);
    const user = userEvent.setup();
    renderShell();

    expect(screen.queryByTestId('app-shell-right')).not.toBeInTheDocument();
    expect(screen.queryByTestId('keyvalues-panel')).not.toBeInTheDocument();

    const toggle = screen.getByTestId('kv-drawer-toggle');
    expect(toggle).toHaveTextContent('Key Values');
    expect(toggle).toHaveAttribute('aria-expanded', 'false');

    await user.click(toggle);
    expect(screen.getByTestId('kv-drawer')).toBeInTheDocument();
    expect(screen.getByTestId('keyvalues-panel')).toBeInTheDocument();
    expect(toggle).toHaveAttribute('aria-expanded', 'true');

    await user.click(screen.getByTestId('kv-drawer-close'));
    expect(screen.queryByTestId('kv-drawer')).not.toBeInTheDocument();
  });

  it('restores the three-column layout when the viewport widens past 999px', async () => {
    const mq = stubMatchMedia(true);
    renderShell();
    expect(screen.queryByTestId('app-shell-right')).not.toBeInTheDocument();

    act(() => mq.setMatches(false));
    expect(screen.getByTestId('app-shell-right')).toBeInTheDocument();
    expect(screen.getByTestId('keyvalues-panel')).toBeInTheDocument();
    expect(screen.queryByTestId('kv-drawer-toggle')).not.toBeInTheDocument();
  });
});
