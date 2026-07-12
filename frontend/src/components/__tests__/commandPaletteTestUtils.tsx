import { render } from '@solidjs/testing-library'
import { vi } from 'vitest'
import { I18nProvider, i18n } from '../../i18n'
import CommandPalette, { type CommandPaletteProps } from '../CommandPalette'

export const defaultPaletteProps: CommandPaletteProps = {
  open: true,
  onClose: vi.fn(),
  onViewChange: vi.fn(),
  onNewDownload: vi.fn(),
  onPauseAll: vi.fn(),
  onResumeAll: vi.fn(),
  onCancelAll: vi.fn(),
  onClearCompleted: vi.fn(),
  onToggleSidebar: vi.fn(),
  getTasks: () => [],
  onOpenTask: vi.fn(),
  getSelectedTask: () => null,
  onOpenTaskFolder: vi.fn(),
  onRedownloadTask: vi.fn(),
  onCopyToClipboard: vi.fn(),
  debounceMs: 0,
}

export function renderPalette(props: Partial<CommandPaletteProps> = {}) {
  return render(() => (
    <I18nProvider i18n={i18n}>
      <CommandPalette {...defaultPaletteProps} {...props} />
    </I18nProvider>
  ))
}

export async function waitForRaf() {
  return new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
}

export async function waitForDebounce() {
  return new Promise<void>((resolve) => setTimeout(() => resolve(), 0))
}
