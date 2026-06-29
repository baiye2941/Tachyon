import { render } from '@solidjs/testing-library'
import { vi } from 'vitest'
import { I18nProvider, i18n } from '../../i18n'
import CommandPalette from '../CommandPalette'

export const defaultPaletteProps = {
  open: true,
  onClose: vi.fn(),
  onViewChange: vi.fn(),
  onNewDownload: vi.fn(),
  onPauseAll: vi.fn(),
  onResumeAll: vi.fn(),
  onToggleSidebar: vi.fn(),
  getTasks: () => [],
  onOpenTask: vi.fn(),
}

export function renderPalette(props: Record<string, unknown> = {}) {
  return render(() => (
    <I18nProvider i18n={i18n}>
      <CommandPalette {...defaultPaletteProps} {...props} />
    </I18nProvider>
  ))
}

export async function waitForRaf() {
  return new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
}
