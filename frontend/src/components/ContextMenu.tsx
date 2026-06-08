import { Show, For } from 'solid-js'
import type { TaskInfo } from '../types'
import {
  PauseIcon, PlayIcon, FolderOpenIcon, LinkIcon,
  RefreshIcon, TrashIcon, InfoIcon,
} from './icons'

interface MenuItem {
  id: string
  label: string
  icon?: () => JSX.Element
  danger?: boolean
  separator?: boolean
  action: () => void
}

interface ContextMenuProps {
  x: number
  y: number
  visible: boolean
  task: TaskInfo | null
  onClose: () => void
  onPause: (taskId: string) => void
  onResume: (taskId: string) => void
  onOpenFolder: (taskId: string) => void
  onCopyLink: (taskId: string) => void
  onRedownload: (taskId: string) => void
  onDelete: (taskId: string) => void
  onDeleteWithFile: (taskId: string) => void
}

import type { JSX } from 'solid-js'

export default function ContextMenu(props: ContextMenuProps) {
  const canPause = () => props.task?.status === 'downloading' || props.task?.status === 'connecting'
  const canResume = () => props.task?.status === 'paused'
  const isCompleted = () => props.task?.status === 'completed'

  const menuItems = (): MenuItem[] => {
    if (!props.task) return []
    const items: MenuItem[] = []

    if (canPause()) {
      items.push({
        id: 'pause',
        label: '暂停',
        icon: () => <PauseIcon />,
        action: () => props.onPause(props.task!.id),
      })
    }
    if (canResume()) {
      items.push({
        id: 'resume',
        label: '恢复',
        icon: () => <PlayIcon />,
        action: () => props.onResume(props.task!.id),
      })
    }

    items.push({ id: 'sep1', label: '', separator: true, action: () => {} })

    if (isCompleted()) {
      items.push({
        id: 'open-folder',
        label: '打开文件所在文件夹',
        icon: () => <FolderOpenIcon />,
        action: () => props.onOpenFolder(props.task!.id),
      })
    }
    items.push({
      id: 'copy-link',
      label: '复制下载链接',
      icon: () => <LinkIcon />,
      action: () => props.onCopyLink(props.task!.id),
    })

    items.push({ id: 'sep2', label: '', separator: true, action: () => {} })

    items.push({
      id: 'redownload',
      label: '重新下载',
      icon: () => <RefreshIcon />,
      action: () => props.onRedownload(props.task!.id),
    })
    items.push({
      id: 'delete',
      label: '删除任务',
      icon: () => <TrashIcon />,
      danger: true,
      action: () => props.onDelete(props.task!.id),
    })
    if (isCompleted()) {
      items.push({
        id: 'delete-with-file',
        label: '删除任务和文件',
        icon: () => <TrashIcon />,
        danger: true,
        action: () => props.onDeleteWithFile(props.task!.id),
      })
    }

    items.push({ id: 'sep3', label: '', separator: true, action: () => {} })

    items.push({
      id: 'properties',
      label: '任务属性',
      icon: () => <InfoIcon />,
      action: () => {},
    })

    return items
  }

  return (
    <Show when={props.visible && props.task}>
      <div
        class="fixed inset-0 z-[150]"
        style={{ background: 'transparent' }}
        onClick={() => props.onClose()}
      />
      <div
        class="fixed z-[160]"
        style={{
          left: `${props.x}px`,
          top: `${props.y}px`,
          'min-width': '180px',
          background: 'rgba(26, 26, 37, 0.95)',
          'backdrop-filter': 'blur(20px) saturate(1.5)',
          'border-radius': '10px',
          border: '1px solid rgba(255, 255, 255, 0.1)',
          'box-shadow': '0 8px 32px rgba(0, 0, 0, 0.4)',
          padding: '6px 0',
          animation: 'fadeIn 100ms ease forwards',
        }}
      >
        <For each={menuItems()}>
          {(item) => (
            <Show
              when={!item.separator}
              fallback={
                <div
                  style={{
                    height: '1px',
                    background: 'rgba(255,255,255,0.08)',
                    margin: '4px 8px',
                  }}
                />
              }
            >
              <button
                class={`flex items-center gap-2 w-full text-left ${item.danger ? 'hover-danger' : 'hover-light'}`}
                style={{
                  height: '32px',
                  padding: '0 12px',
                  'font-size': '14px',
                  color: item.danger ? '#EF4444' : '#F0F0F5',
                  background: 'transparent',
                  border: 'none',
                  cursor: 'pointer',
                  transition: 'all 150ms ease',
                  'border-radius': '4px',
                  margin: '0 4px',
                  width: 'calc(100% - 8px)',
                }}
                onClick={() => {
                  item.action()
                  props.onClose()
                }}
              >
                <span style={{ width: '16px', height: '16px', display: 'flex', 'align-items': 'center' }}>
                  {item.icon?.()}
                </span>
                <span>{item.label}</span>
              </button>
            </Show>
          )}
        </For>
      </div>
    </Show>
  )
}
