import tseslint from '@typescript-eslint/eslint-plugin'
import tsparser from '@typescript-eslint/parser'
import solidPlugin from 'eslint-plugin-solid'

export default [
  {
    files: ['src/**/*.{ts,tsx}'],
    languageOptions: {
      parser: tsparser,
      parserOptions: {
        ecmaVersion: 'latest',
        sourceType: 'module',
        jsxPragma: 'h',
      },
    },
    plugins: {
      '@typescript-eslint': tseslint,
      solid: solidPlugin,
    },
    rules: {
      ...tseslint.configs.recommended.rules,
      ...solidPlugin.configs.recommended.rules,
      '@typescript-eslint/no-unused-vars': ['warn', { argsIgnorePattern: '^_' }],
      'solid/reactivity': 'warn',
      'solid/no-destructure': 'error',
    },
  },
]
