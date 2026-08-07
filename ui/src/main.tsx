import React from 'react';
import ReactDOM from 'react-dom/client';
import './i18n';
import './styles/theme.css';
import { SessionProvider, getInitialTheme } from './state/session';
import AppShell from './components/AppShell';

document.documentElement.dataset.theme = getInitialTheme();

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <SessionProvider>
      <AppShell />
    </SessionProvider>
  </React.StrictMode>,
);
