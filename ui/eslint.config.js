import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import tseslint from 'typescript-eslint'

export default tseslint.config(
  { ignores: ['dist'] },
  {
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
    },
    plugins: {
      'react-hooks': reactHooks,
      'react-refresh': reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      'react-refresh/only-export-components': ['warn', { allowConstantExport: true }],
      // UI code must not touch the Tauri API directly; only src/ipc/real.ts may (ipc-ui.md §3.4).
      'no-restricted-imports': [
        'error',
        {
          paths: [{ name: '@tauri-apps/api', message: 'UI code must not import @tauri-apps/api; go through src/ipc' }],
        },
      ],
      // Theme values live only in src/styles/theme.css as CSS variables; raw hex in TSX is forbidden (ipc-ui.md §7).
      'no-restricted-syntax': [
        'error',
        {
          selector: 'Literal[value=/^#[0-9a-fA-F]{3,8}$/]',
          message: 'No raw color literals in components; use var(--ab-*) theme tokens',
        },
      ],
    },
  },
  {
    // Real IPC binding is the single sanctioned @tauri-apps/api consumer (ipc-ui.md §3.2 module layout).
    files: ['src/ipc/real.ts'],
    rules: {
      'no-restricted-imports': 'off',
    },
  },
  {
    // session.ts mixes a provider component with hooks and pure functions by design (Context + useReducer).
    files: ['src/state/session.ts'],
    rules: {
      'react-refresh/only-export-components': 'off',
    },
  },
  {
    // useMockIpc is a plain env-switch function (ipc-ui.md §3.2), not a React hook despite the spec name.
    files: ['src/ipc/ipc.ts'],
    rules: {
      'react-hooks/rules-of-hooks': 'off',
    },
  },
)
