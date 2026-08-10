import React from 'react';
import ReactDOM from 'react-dom/client';
import './i18n';
import './styles/theme.css';
import { SessionProvider, getInitialTheme } from './state/session';
import AppShell from './components/AppShell';
import ErrorBoundary from './components/ErrorBoundary';
import { installGlobalErrorHandlers } from './lib/globalErrors';

document.documentElement.dataset.theme = getInitialTheme();

// 全局错误屏显 + 持久日志（window.onerror / unhandledrejection，任务 17 正式功能）。
installGlobalErrorHandlers();

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <SessionProvider>
        <AppShell />
      </SessionProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);
