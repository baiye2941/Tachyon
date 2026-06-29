import { createSignal, For, Show } from 'solid-js'
import type { HubFileInfo, FileCategory, FileVerifyResult } from '../types'
import { groupFilesByCategory } from '../utils/hfTree'
import { formatSize } from '../utils/format'
import { CheckboxIcon, ChevronDownIcon } from './icons'
import { tr } from '../i18n'

interface ModelFileTreeProps {
  files: HubFileInfo[]
  selectedPaths: Set<string>
  verifyResults?: Record<string, FileVerifyResult>
  onToggleSelection: (path: string) => void
  readOnly?: boolean
}

const CATEGORY_ORDER: FileCategory[] = [
  'modelWeight',
  'config',
  'tokenizer',
  'code',
  'data',
  'document',
  'other',
]

const CATEGORY_I18N = {
  modelWeight: 'hub.fileTree.modelWeight',
  config: 'hub.fileTree.config',
  tokenizer: 'hub.fileTree.tokenizer',
  code: 'hub.fileTree.code',
  data: 'hub.fileTree.data',
  document: 'hub.fileTree.document',
  other: 'hub.fileTree.other',
} as const

/** 获取校验状态显示文本与颜色 */
function getVerifyDisplay(result?: FileVerifyResult): { label: string; color: string } {
  if (!result) return { label: tr('hub.verify.unverified'), color: 'var(--color-text-tertiary)' }
  const status = result.status
  if (status === 'verified') {
    return { label: tr('hub.verify.verified'), color: 'var(--color-status-completed)' }
  }
  if (status === 'unverified') {
    return { label: tr('hub.verify.unverified'), color: 'var(--color-text-tertiary)' }
  }
  return { label: tr('hub.verify.failed'), color: 'var(--color-status-error)' }
}

/** 单个分类区块组件(内部管理展开/折叠状态) */
function CategorySection(props: {
  category: FileCategory
  files: HubFileInfo[]
  selectedPaths: Set<string>
  verifyResults?: Record<string, FileVerifyResult>
  onToggleSelection: (path: string) => void
  readOnly: boolean
}) {
  const [expanded, setExpanded] = createSignal(true)
  const label = () => tr(CATEGORY_I18N[props.category])
  const count = () => props.files.length

  return (
    <div
      style={{
        'border-radius': '8px',
        border: '1px solid var(--color-border-subtle)',
        overflow: 'hidden',
        'margin-bottom': '8px',
      }}
    >
      {/* Category header */}
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        class="flex items-center w-full"
        style={{
          padding: '8px 12px',
          background: 'var(--color-bg-elevated)',
          cursor: 'pointer',
          border: 'none',
          outline: 'none',
        }}
      >
        <div
          style={{
            width: '12px',
            height: '12px',
            transform: expanded() ? 'rotate(0deg)' : 'rotate(-90deg)',
            transition: 'transform 150ms ease',
            color: 'var(--color-text-tertiary)',
            'margin-right': '8px',
            'flex-shrink': 0,
          }}
        >
          <ChevronDownIcon />
        </div>
        <span
          style={{
            'font-size': '13px',
            'font-weight': 600,
            color: 'var(--color-text-title)',
            flex: 1,
            'text-align': 'left',
          }}
        >
          {label()}
        </span>
        <span
          style={{
            'font-size': '11px',
            color: 'var(--color-text-tertiary)',
            'flex-shrink': 0,
          }}
        >
          {count()} 个文件
        </span>
      </button>

      {/* File list */}
      <Show when={expanded()}>
        <For each={props.files}>
          {(file) => {
            const isSelected = props.selectedPaths.has(file.path)
            const verifyResult = props.readOnly
              ? props.verifyResults?.[file.path]
              : undefined
            const verifyDisplay = getVerifyDisplay(verifyResult)

            return (
              <div
                class="flex items-center"
                style={{
                  padding: '6px 12px',
                  'border-top': '1px solid var(--color-border-subtle)',
                  'min-height': '32px',
                }}
              >
                {/* Checkbox */}
                <Show when={!props.readOnly}>
                  <div
                    class="flex-shrink-0 cursor-pointer"
                    style={{
                      width: '16px',
                      height: '16px',
                      color: isSelected
                        ? 'var(--color-accent-primary)'
                        : 'var(--color-text-tertiary)',
                      'margin-right': '8px',
                    }}
                    onClick={() => props.onToggleSelection(file.path)}
                  >
                    <CheckboxIcon checked={isSelected} />
                  </div>
                </Show>

                {/* File name */}
                <span
                  class="flex-1 truncate"
                  style={{
                    'font-size': '13px',
                    color: 'var(--color-text-primary)',
                    'margin-right': '8px',
                  }}
                >
                  {file.path.split('/').pop() ?? file.path}
                </span>

                {/* LFS badge */}
                <Show when={file.lfs}>
                  <span
                    class="flex-shrink-0"
                    style={{
                      'font-size': '10px',
                      'font-weight': 600,
                      color: 'var(--color-accent-primary)',
                      background: 'var(--color-accent-soft)',
                      padding: '1px 4px',
                      'border-radius': '3px',
                      'margin-right': '8px',
                    }}
                  >
                    LFS
                  </span>
                </Show>

                {/* Verify status (readOnly mode) */}
                <Show when={props.readOnly}>
                  <span
                    class="flex-shrink-0"
                    style={{
                      'font-size': '11px',
                      color: verifyDisplay.color,
                      'margin-right': '8px',
                    }}
                  >
                    {verifyDisplay.label}
                  </span>
                </Show>

                {/* File size */}
                <span
                  class="flex-shrink-0 mono"
                  style={{
                    'font-size': '11px',
                    color: 'var(--color-text-tertiary)',
                    'margin-right': '8px',
                  }}
                >
                  {formatSize(file.size ?? file.lfs?.size ?? null)}
                </span>
              </div>
            )
          }}
        </For>
      </Show>
    </div>
  )
}

export default function ModelFileTree(props: ModelFileTreeProps) {
  const grouped = () => groupFilesByCategory(props.files)

  return (
    <div style={{ width: '100%' }}>
      <For each={CATEGORY_ORDER}>
        {(category) => {
          const files = () => grouped()[category] ?? []
          return (
            <Show when={files().length > 0}>
              <CategorySection
                category={category}
                files={files()}
                selectedPaths={props.selectedPaths}
                verifyResults={props.verifyResults}
                onToggleSelection={props.onToggleSelection}
                readOnly={props.readOnly ?? false}
              />
            </Show>
          )
        }}
      </For>
    </div>
  )
}
