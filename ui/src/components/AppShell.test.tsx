import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import { SessionProvider } from '../state/session';
import AppShell from './AppShell';

describe('AppShell (ipc-ui.md §4.1)', () => {
  afterEach(() => {
    window.location.hash = '';
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
