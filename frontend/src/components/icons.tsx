/* Tachyon Icon System - Phosphor path 内嵌版
 * path data 来源: @phosphor-icons/core@2.1.1 (MIT License)
 *   https://github.com/phosphor-icons/web
 * 仅内嵌 path d 字符串(无颜色字面量),合规 lint:colors。
 * viewBox 统一 0 0 256 256(Phosphor 规约),fill="currentColor"。
 * duotone 图标(文件类)用双层 path:底层 opacity 0.2 + 主轮廓。 */

import { regular as P, duotone as D } from "./iconPaths";
import type { JSX } from "solid-js";

/** 图标尺寸 prop:可由调用方覆盖工厂默认值 */
interface IconProps {
  class?: string;
  /** 覆盖默认尺寸(px),优先于 Tailwind w-/h- 类(inline 优先级最高) */
  size?: number;
}

/** regular 单 path 工厂。
 *  fill="currentColor",颜色随父容器 color 驱动。
 *  size 默认 16,可经 props.size 覆盖。 */
function makeRegular(pathKey: keyof typeof P, defaultSize = 16) {
  return function IconCmp(props: IconProps): JSX.Element {
    return (
      <svg
        class={props.class}
        width={props.size ?? defaultSize}
        height={props.size ?? defaultSize}
        viewBox="0 0 256 256"
        fill="currentColor"
        xmlns="http://www.w3.org/2000/svg"
        aria-hidden="true"
        style={{ "flex-shrink": "0" }}
      >
        <path d={P[pathKey]} />
      </svg>
    );
  };
}

/** duotone 双 path 工厂(文件类材质质感)。
 *  底层 opacity 0.2 + 主轮廓 fill="currentColor"。 */
function makeDuotone(
  pathKey: keyof typeof D,
  defaultSize = 20,
) {
  return function IconCmp(props: IconProps): JSX.Element {
    const d = D[pathKey];
    return (
      <svg
        class={props.class}
        width={props.size ?? defaultSize}
        height={props.size ?? defaultSize}
        viewBox="0 0 256 256"
        fill="currentColor"
        xmlns="http://www.w3.org/2000/svg"
        aria-hidden="true"
        style={{ "flex-shrink": "0" }}
      >
        {/* 底层副色(Phosphor 规约 opacity 0.2) */}
        <path d={d.layer} opacity="0.2" />
        {/* 主轮廓 */}
        <path d={d.main} />
      </svg>
    );
  };
}

/* ===== 操作类(regular,18px:高 DPI 屏可读性)===== */
export const PlusIcon = makeRegular("plus", 18);
export const DownloadSimpleIcon = makeRegular("downloadSimple", 18);
export const SearchIcon = makeRegular("magnifyingGlass", 18);
export const PauseIcon = makeRegular("pause", 18);
export const PlayIcon = makeRegular("play", 18);
export const CancelIcon = makeRegular("stop", 18);
export const TrashIcon = makeRegular("trash", 18);
export const SettingsIcon = makeRegular("gearSix", 18);
export const XIcon = makeRegular("x", 18);
export const CloseIcon = makeRegular("x", 14);
export const MinimizeIcon = makeRegular("minus", 14);
export const MaximizeIcon = makeRegular("squaresFour", 14);
export const RestoreIcon = makeRegular("squaresFour", 14);
export const MenuIcon = makeRegular("list", 18);
export const FilterIcon = makeRegular("funnelSimple", 18);
export const FunnelSimpleIcon = makeRegular("funnelSimple", 18);
export const CaretDoubleLeftIcon = makeRegular("caretDoubleLeft", 18);
export const WarningCircleIcon = makeRegular("warningCircle", 18);
export const StackIcon = makeRegular("stack", 18);
export const HashIcon = makeRegular("hash", 18);
export const CheckCircleIcon = makeRegular("checkCircle", 18);
export const LightningIcon = makeRegular("lightning", 18);
export const ArrowLeftIcon = makeRegular("arrowLeft", 18);
export const ArrowDownIcon = makeRegular("arrowDown", 14);
export const ChevronDownIcon = makeRegular("caretDown", 14);
export const RefreshIcon = makeRegular("arrowClockwise", 18);
export const CopyIcon = makeRegular("copy", 18);
export const FolderOpenIcon = makeRegular("folderOpen", 18);
export const LinkIcon = makeRegular("link", 18);
export const InfoIcon = makeRegular("info", 18);
export const EditIcon = makeRegular("pencilSimple", 18);
export const MoreIcon = makeRegular("dotsThree", 18);
export const OpenFileIcon = makeRegular("arrowSquareOut", 18);
export const GridFourIcon = makeRegular("gridFour", 18);
export const ListIcon = makeRegular("listBullets", 20);
export const ListBulletsIcon = makeRegular("listBullets", 20);

/* ===== 导航/状态类(regular,22px)===== */
export const GearIcon = makeRegular("gear", 22);
export const FileIcon = makeRegular("fileText", 22);
export const BrowserIcon = makeRegular("browser", 22);
export const RadarIcon = makeRegular("broadcast", 22);
export const HistoryIcon = makeRegular("clock", 22);
export const ClockIcon = makeRegular("clock", 22);
export const TrophyIcon = makeRegular("trophy", 22);
export const GlobeIcon = makeRegular("globe", 22);
export const PackageIcon = makeRegular("package", 22);
export const ChartBarIcon = makeRegular("chartBar", 22);
export const PauseCircleIcon = makeRegular("pauseCircle", 22);
export const CheckIcon = makeRegular("check", 18);
export const StarIcon = makeRegular("star", 18);
export const SunIcon = makeRegular("sun", 18);
export const MoonIcon = makeRegular("moon", 18);

/* ===== 文件类型(duotone,22px,材质质感)===== */
export const VideoIcon = makeDuotone("fileVideo", 22);
export const AudioIcon = makeDuotone("fileAudio", 22);
export const DocumentIcon = makeDuotone("fileText", 22);
export const ImageIcon = makeDuotone("fileImage", 22);
export const ArchiveIcon = makeDuotone("fileArchive", 22);
// 代码文件复用 fileCode
export const CodeFileIcon = makeDuotone("fileCode", 22);
// 附件/其他文件用 regular fileText(无 duotone 区分,降级)
export const AttachmentIcon = makeRegular("fileText", 22);

/* ===== Hub 图标(模型仓库,叠层)===== */
export const HubIcon = makeRegular("stack", 20);

/* ===== MoveIcon(移动到标签,用 arrows-out-of-center 近似)=====
 * Phosphor 无精确 move,用 grid-four 占位语义保持导出契约 */
export const MoveIcon = makeRegular("gridFour", 16);

/* ===== 特殊保留图标(非 Phosphor,保持原规约)===== */

/** Tachyon Logo:快子几何标识(品牌,保留自绘) */
export const LogoIcon = (props: IconProps) => (
  <svg
    class={props.class}
    width="18"
    height="18"
    viewBox="0 0 24 24"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M4 12L12 4L20 12L12 20L4 12Z"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
    <circle cx="12" cy="12" r="3" fill="currentColor" />
    <path
      d="M2 12H6M18 12H22M12 2V6M12 18V22"
      stroke="currentColor"
      stroke-width="1"
      stroke-linecap="round"
      opacity="0.35"
    />
  </svg>
);

/** StatusDot:状态小圆点(8x8,非 Phosphor) */
export const StatusDot = (props: { class?: string; active?: boolean }) => (
  <svg class={props.class} width="8" height="8" viewBox="0 0 8 8" aria-hidden="true">
    <circle
      cx="4"
      cy="4"
      r="4"
      fill={
        props.active
          ? "var(--color-accent-primary)"
          : "var(--color-text-tertiary)"
      }
    />
  </svg>
);

/**
 * CheckboxIcon:复选框。
 * 保留 24-viewBox + checkmark path d="M9 12l2 2 4-4"(HfBrowserPanel.spec 断言此值,
 * 不可改)。checked 时实心 + 白勾,未 checked 时空心框。
 */
export const CheckboxIcon = (props: {
  class?: string;
  checked?: boolean;
  indeterminate?: boolean;
}) => (
  <svg
    class={props.class}
    width="16"
    height="16"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    {props.checked || props.indeterminate ? (
      <>
        <rect
          x="3"
          y="3"
          width="18"
          height="18"
          rx="2"
          fill="currentColor"
          stroke="none"
        />
        {props.indeterminate ? (
          <path d="M7 12h10" stroke="white" stroke-width="2" />
        ) : (
          <path d="M9 12l2 2 4-4" stroke="white" stroke-width="2" />
        )}
      </>
    ) : (
      <rect x="3" y="3" width="18" height="18" rx="2" />
    )}
  </svg>
);

/** SelectIcon:多选切换(网格 + 勾选语义,24-viewBox 保留) */
export const SelectIcon = (props: IconProps) => (
  <svg
    class={props.class}
    width="16"
    height="16"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <path d="M3 9h18M9 3v18" />
  </svg>
);

/** PinIcon / PinOffIcon:侧栏钉住(保留自绘,Phosphor 无贴切图) */
export const PinIcon = (props: IconProps) => (
  <svg
    class={props.class}
    width="16"
    height="16"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path d="M12 2v8M5 10h14l-2.5 7h-9L5 10zM12 17v5" />
  </svg>
);

export const PinOffIcon = (props: IconProps) => (
  <svg
    class={props.class}
    width="16"
    height="16"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path d="M12 2v8M5 10h14l-2.5 7h-9L5 10zM12 17v5" />
    <path d="M2 2l20 20" />
  </svg>
);

/** LoadingIcon:加载旋转(保留 spin 动画) */
export const LoadingIcon = (props: IconProps) => (
  <svg
    class={props.class}
    width="16"
    height="16"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <style>{`
      @keyframes spin { 100% { transform: rotate(360deg); } }
    `}</style>
    <g
      style={{
        animation: "spin 1s linear infinite",
        "transform-origin": "center",
      }}
    >
      <path
        d="M12 2v4M12 18v4M4.93 4.93l2.83 2.83M16.24 16.24l2.83 2.83M2 12h4M18 12h4M4.93 19.07l2.83-2.83M16.24 7.76l2.83-2.83"
        opacity="0.3"
      />
      <path d="M12 2v4" />
    </g>
  </svg>
);
