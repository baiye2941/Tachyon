/**
 * 统一图标系统
 *
 * 使用 Heroicons 20 Solid 风格的 SVG path。
 * 所有图标基于 20x20 viewBox，fill="currentColor"。
 * 通过 <Icon name="..." /> 组件统一渲染。
 *
 * 旧版导出 (IconPause/IconResume/IconCancel/IconDelete) 保留兼容，
 * 内部委托给统一的 <Icon /> 组件。
 */

import { type JSX, splitProps } from 'solid-js'

// ---- Heroicons 20 Solid path 数据 ----

const PATHS: Record<string, string> = {
  // 导航
  'list-bullet': 'M3 4.5A1.5 1.5 0 014.5 3h11A1.5 1.5 0 0117 4.5v11a1.5 1.5 0 01-1.5 1.5h-11A1.5 1.5 0 013 15.5v-11z',
  'arrow-down-tray': 'M3 17a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1zm3.293-7.707a1 1 0 011.414 0L9 10.586V3a1 1 0 112 0v7.586l1.293-1.293a1 1 0 111.414 1.414l-3 3a1 1 0 01-1.414 0l-3-3a1 1 0 010-1.414z',
  'check-circle': 'M10 18a8 8 0 100-16 8 8 0 000 16zm3.857-9.807a.75.75 0 00-1.214-.882l-3.483 4.79-1.88-1.88a.75.75 0 10-1.06 1.061l2.5 2.5a.75.75 0 001.137-.089l4-5.5z',
  'clock': 'M10 18a8 8 0 100-16 8 8 0 000 16zm.75-13a.75.75 0 00-1.5 0v5c0 .414.336.75.75.75h4a.75.75 0 000-1.5h-3.25V5z',
  'magnifying-glass': 'M9 3.5a5.5 5.5 0 100 11 5.5 5.5 0 000-11zM2 9a7 7 0 1112.452 4.391l3.328 3.329a.75.75 0 11-1.06 1.06l-3.329-3.328A7 7 0 012 9z',
  'cog-6-tooth': 'M7.84 1.804A1 1 0 018.82 1h2.36a1 1 0 01.98.804l.331 1.652a6.993 6.993 0 011.929 1.115l1.598-.54a1 1 0 011.186.447l1.18 2.044a1 1 0 01-.205 1.251l-1.267 1.113a7.047 7.047 0 010 2.228l1.267 1.113a1 1 0 01.206 1.25l-1.18 2.045a1 1 0 01-1.187.447l-1.598-.54a6.993 6.993 0 01-1.929 1.115l-.33 1.652a1 1 0 01-.982.804H8.82a1 1 0 01-.98-.804l-.331-1.652a6.993 6.993 0 01-1.929-1.115l-1.598.54a1 1 0 01-1.186-.447l-1.18-2.044a1 1 0 01.205-1.251l1.267-1.114a7.05 7.05 0 010-2.227L1.821 7.773a1 1 0 01-.206-1.25l1.18-2.045a1 1 0 011.187-.447l1.598.54A6.992 6.992 0 017.51 3.456l.33-1.652zM10 13a3 3 0 100-6 3 3 0 000 6z',
  'chart-bar': 'M15.5 3.5v13a.5.5 0 01-.5.5H5a.5.5 0 01-.5-.5v-13a.5.5 0 01.5-.5h10a.5.5 0 01.5.5zM6.5 12h2V8h-2v4zm3 0h2V5h-2v7zm3 0h2v-6h-2v6z',

  // 操作
  'pause': 'M5.75 3a.75.75 0 00-.75.75v12.5c0 .414.336.75.75.75h1.5a.75.75 0 00.75-.75V3.75A.75.75 0 007.25 3h-1.5zm7.5 0a.75.75 0 00-.75.75v12.5c0 .414.336.75.75.75h1.5a.75.75 0 00.75-.75V3.75a.75.75 0 00-.75-.75h-1.5z',
  'play': 'M4.5 3.75a.75.75 0 00-.75.75v11c0 .414.336.75.75.75h.75a.75.75 0 00.472-.166l9.5-6.5a.75.75 0 000-1.168l-9.5-6.5A.75.75 0 005.25 2.25h-.75z',
  'x-mark': 'M6.28 5.22a.75.75 0 00-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 101.06 1.06L10 11.06l3.72 3.72a.75.75 0 101.06-1.06L11.06 10l3.72-3.72a.75.75 0 00-1.06-1.06L10 8.94 6.28 5.22z',
  'trash': 'M8.75 1A2.75 2.75 0 006 3.75v.443c-.795.077-1.584.176-2.365.298a.75.75 0 10.23 1.482l.149-.022.841 10.518A2.75 2.75 0 007.596 19h4.807a2.75 2.75 0 002.742-2.53l.841-10.52.149.023a.75.75 0 00.23-1.482A41.03 41.03 0 0014 4.193V3.75A2.75 2.75 0 0011.25 1h-2.5zM10 4c.84 0 1.673.025 2.5.075V3.75c0-.69-.56-1.25-1.25-1.25h-2.5c-.69 0-1.25.56-1.25 1.25v.325C8.327 4.025 9.16 4 10 4zM8.58 7.72a.75.75 0 00-1.5.06l.3 7.5a.75.75 0 101.5-.06l-.3-7.5zm4.34.06a.75.75 0 10-1.5-.06l-.3 7.5a.75.75 0 101.5.06l.3-7.5z',
  'plus': 'M10 3a1 1 0 011 1v5h5a1 1 0 110 2h-5v5a1 1 0 11-2 0v-5H4a1 1 0 110-2h5V4a1 1 0 011-1z',
  'pause-circle': 'M10 18a8 8 0 100-16 8 8 0 000 16zM8 7a1 1 0 00-1 1v4a1 1 0 001 1h1a1 1 0 001-1V8a1 1 0 00-1-1H8zm3 0a1 1 0 00-1 1v4a1 1 0 001 1h1a1 1 0 001-1V8a1 1 0 00-1-1h-1z',

  // 窗口控制
  'minus': 'M4 10a.75.75 0 01.75-.75h10.5a.75.75 0 010 1.5H4.75A.75.75 0 014 10z',
  'square': 'M4.5 3A1.5 1.5 0 003 4.5v11A1.5 1.5 0 004.5 17h11a1.5 1.5 0 001.5-1.5v-11A1.5 1.5 0 0015.5 3h-11zM4.5 4.5h11v11h-11v-10z',
  'window': 'M3.5 3h13A1.5 1.5 0 0118 4.5v10a1.5 1.5 0 01-1.5 1.5h-13A1.5 1.5 0 012 14.5v-10A1.5 1.5 0 013.5 3zm0 1.5v10h13v-10h-13z',

  // 状态/方向
  'chevron-right': 'M7.21 14.77a.75.75 0 01.02-1.06L11.168 10 7.23 6.29a.75.75 0 111.04-1.08l4.5 4.25a.75.75 0 010 1.08l-4.5 4.25a.75.75 0 01-1.06-.02z',
  'ellipsis-vertical': 'M10 3a1.5 1.5 0 110 3 1.5 1.5 0 010-3zm0 5.5a1.5 1.5 0 110 3 1.5 1.5 0 010-3zm0 5.5a1.5 1.5 0 110 3 1.5 1.5 0 010-3z',
}

// ---- 通用图标组件 ----

type IconProps = JSX.SvgSVGAttributes<SVGSVGElement> & {
  name: string
  class?: string
}

export function Icon(props: IconProps) {
  const [, rest] = splitProps(props, ['name', 'class'])
  const d = () => PATHS[props.name]

  return (
    <>
      {d() ? (
        <svg
          viewBox="0 0 20 20"
          fill="currentColor"
          class={props.class}
          aria-hidden="true"
          {...rest}
        >
          <path fill-rule="evenodd" d={d()!} clip-rule="evenodd" />
        </svg>
      ) : import.meta.env.DEV ? (
        (() => { console.warn(`[Icon] 未知图标: ${props.name}`); return null })()
      ) : null}
    </>
  )
}

// ---- 旧版兼容导出 ----
// DownloadCard / BatchToolbar 使用的旧接口

interface LegacyIconProps {
  class?: string
}

export function IconPause(props: LegacyIconProps) {
  return <Icon name="pause" class={`w-4 h-4 ${props.class || ''}`} />
}

export function IconResume(props: LegacyIconProps) {
  return <Icon name="play" class={`w-4 h-4 ${props.class || ''}`} />
}

export function IconCancel(props: LegacyIconProps) {
  return <Icon name="x-mark" class={`w-4 h-4 ${props.class || ''}`} />
}

export function IconDelete(props: LegacyIconProps) {
  return <Icon name="trash" class={`w-4 h-4 ${props.class || ''}`} />
}

/** 获取图标 path 数据（用于 Sidebar 等直接渲染 SVG path 的场景） */
export function getIconPath(name: string): string | undefined {
  return PATHS[name]
}

/** 所有已注册的图标名称 */
export const ICON_NAMES = Object.keys(PATHS) as readonly string[]
