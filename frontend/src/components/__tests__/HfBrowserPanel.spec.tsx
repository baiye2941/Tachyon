import { describe, it, expect, vi, afterEach } from 'vitest'
import { createSignal } from 'solid-js'
import { render, cleanup, fireEvent, screen } from '@solidjs/testing-library'
import { I18nProvider, i18n } from '../../i18n'
import type { HubFileInfo } from '../../types'
import HfBrowserPanel from '../HfBrowserPanel'

const [repoFiles, setRepoFiles] = createSignal<HubFileInfo[]>([])
const [loading] = createSignal(false)
const [error] = createSignal<string | null>(null)

const nestedRepoFiles: HubFileInfo[] = [
  { path: '.eval_results', size: 0, type: 'directory', lfs: null },
  { path: '.eval_results/deep-swe.yaml', size: 153, type: 'file', lfs: null },
  { path: '.gitattributes', size: 1536, type: 'file', lfs: null },
]

vi.mock('../../stores/hub', () => ({
  $hub: {
    get repoFiles() { return repoFiles },
    get loading() { return loading },
    get error() { return error },
  },
  listRepoFiles: vi.fn(),
  clearRepoFiles: vi.fn(() => setRepoFiles([])),
}))

vi.mock('../../api/invoke', () => ({
  api: {
    getHfDownloadUrl: vi.fn(),
    createTask: vi.fn(),
  },
}))

vi.mock('../../stores/toast', () => ({
  addToast: vi.fn(),
}))

vi.mock('../../stores/downloads', () => ({
  refreshTaskList: vi.fn(),
}))

const renderPanel = () => render(() => (
  <I18nProvider i18n={i18n}>
    <HfBrowserPanel visible onClose={() => {}} />
  </I18nProvider>
))

describe('HfBrowserPanel', () => {
  afterEach(() => {
    cleanup()
    setRepoFiles([])
    vi.clearAllMocks()
  })

  it('打开面板时不应因 repoFiles 初始化顺序崩溃', () => {
    expect(() => renderPanel()).not.toThrow()
  })

  it('嵌套目录文件勾选后应该显示选中态', async () => {
    setRepoFiles(nestedRepoFiles)
    renderPanel()

    fireEvent.input(screen.getByPlaceholderText(/owner\/repo/), {
      target: { value: 'zai-org/GLM-5.2' },
      currentTarget: { value: 'zai-org/GLM-5.2' },
    })
    fireEvent.click(screen.getByText('浏览'))

    const nestedFile = await screen.findByText('deep-swe.yaml')
    const nestedCheckbox = nestedFile.parentElement!.querySelector('svg')!
    fireEvent.click(nestedCheckbox)

    expect(nestedFile.parentElement?.getAttribute('aria-selected')).toBe('true')
    expect(nestedCheckbox.querySelector('path[d="M9 12l2 2 4-4"]')).not.toBeNull()
  })
})
