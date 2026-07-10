/**
 * 统一图标系统代理层
 *
 * 通过 name 映射到 components/icons.tsx 中的图标组件。
 * 禁止在此文件维护 SVG path 数据。
 */

import { type JSX } from "solid-js";
import {
  PlusIcon,
  PauseIcon,
  PlayIcon,
  TrashIcon,
  SearchIcon,
  GearIcon,
  ClockIcon,
  ListIcon,
  ChartBarIcon,
  PauseCircleIcon,
  CancelIcon,
  CopyIcon,
  FolderOpenIcon,
  RefreshIcon,
  PinIcon,
  PinOffIcon,
} from "../components/icons";

type IconComponent = (props: { class?: string }) => JSX.Element;

const ICON_MAP: Record<string, IconComponent> = {
  plus: PlusIcon,
  pause: PauseIcon,
  play: PlayIcon,
  trash: TrashIcon,
  "list-bullet": ListIcon,
  "magnifying-glass": SearchIcon,
  "cog-6-tooth": GearIcon,
  clock: ClockIcon,
  "chart-bar": ChartBarIcon,
  "pause-circle": PauseCircleIcon,
  cancel: CancelIcon,
  copy: CopyIcon,
  "folder-open": FolderOpenIcon,
  refresh: RefreshIcon,
  pin: PinIcon,
  "pin-off": PinOffIcon,
};

export interface IconProps extends JSX.SvgSVGAttributes<SVGSVGElement> {
  name: string;
  class?: string;
}

export function Icon(props: IconProps) {
  return (
    <>
      {() => {
        const Component = ICON_MAP[props.name];

        if (!Component) {
          if (import.meta.env.DEV) {
            console.warn(`[Icon] 未知图标: ${props.name}`);
          }
          return null;
        }

        return <Component class={props.class} />;
      }}
    </>
  );
}

/** 获取图标 path 数据（保留兼容，当前实现返回 undefined） */
export function getIconPath(_name: string): string | undefined {
  return undefined;
}

/** 所有已注册的图标名称 */
export const ICON_NAMES = Object.keys(ICON_MAP) as readonly string[];
