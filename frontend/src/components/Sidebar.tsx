import { Show, For, untrack, createMemo } from "solid-js";
import type { JSX } from "solid-js";
import type { SidebarFilter, FileTypeFilter } from "../types";
import {
  VideoIcon,
  AudioIcon,
  DocumentIcon,
  ImageIcon,
  ArchiveIcon,
  AttachmentIcon,
  PinIcon,
  PinOffIcon,
  HistoryIcon,
  RadarIcon,
  HubIcon,
  StackIcon,
  DownloadSimpleIcon,
  CheckCircleIcon,
  PauseIcon,
  WarningCircleIcon,
} from "./icons";
import {
  $taskFilter,
  toggleSidebarFilter,
  toggleFileTypeFilter,
} from "../stores/taskFilter";
import {
  $ui,
  SIDEBAR_RAIL_WIDTH as RAIL_WIDTH,
  SIDEBAR_MIN_EXPANDED_WIDTH as MIN_EXPANDED_WIDTH,
  SIDEBAR_MAX_WIDTH as MAX_WIDTH,
} from "../stores/ui";
import Button from "../shared/ui/Button";
import { $experimental } from "../stores/experimental";
import { useReducedMotion } from "../hooks/useReducedMotion";
import { useIsNarrowScreen } from "../hooks/useMediaQuery";
import { tr, type MessageKey } from "../i18n";

const EDGE_ZONE_WIDTH = 6;
const HOVER_COLLAPSE_DELAY = 240;
type IconComponent = (props: { class?: string }) => JSX.Element;

interface NavEntry {
  key: SidebarFilter;
  labelKey: MessageKey;
  icon: IconComponent;
}

const statusItems: NavEntry[] = [
  { key: "all", labelKey: "sidebar.filter.all", icon: StackIcon },
  { key: "downloading", labelKey: "status.label.downloading", icon: DownloadSimpleIcon },
  { key: "completed", labelKey: "status.label.completed", icon: CheckCircleIcon },
  { key: "paused", labelKey: "status.label.paused", icon: PauseIcon },
  { key: "failed", labelKey: "sidebar.filter.failed", icon: WarningCircleIcon },
];

const typeItems: { key: FileTypeFilter; labelKey: MessageKey; icon: IconComponent }[] =
  [
    { key: "video", labelKey: "sidebar.filter.video", icon: VideoIcon },
    { key: "audio", labelKey: "sidebar.filter.audio", icon: AudioIcon },
    { key: "document", labelKey: "sidebar.filter.document", icon: DocumentIcon },
    { key: "image", labelKey: "sidebar.filter.image", icon: ImageIcon },
    { key: "archive", labelKey: "sidebar.filter.archive", icon: ArchiveIcon },
    { key: "other", labelKey: "sidebar.filter.other", icon: AttachmentIcon },
  ];

type NavItemProps = {
  icon: IconComponent;
  labelKey: MessageKey;
  count: number;
  active: boolean;
  expanded: boolean;
  onClick: () => void;
  badge?: string;
};

const NavItem = (p: NavItemProps) => {
  const Icon = untrack(() => p.icon);
  const label = () => tr(p.labelKey);
  return (
    <button
      class={`sidebar-nav-item flex items-center cursor-pointer select-none focus:outline-none focus-visible:focus-ring relative ${p.active ? "is-active" : "hover-light"}`}
      style={{
        height: "34px",
        padding: p.expanded ? "0 12px" : "0",
        "border-radius": "7px",
        background: p.active ? "var(--color-surface-2, var(--color-bg-tertiary))" : "transparent",
        color: p.active
          ? "var(--color-text-primary)"
          : "var(--color-text-secondary)",
        transition: "background 150ms ease, color 150ms ease",
        "justify-content": p.expanded ? "space-between" : "center",
        border: "none",
        width: "100%",
        /* active 态:顶光 inset + 微下沉阴影(参考稿 raised active) */
        "box-shadow": p.active
          ? "inset 0 1px 0 var(--color-inset-dark), inset 0 0 0 0.5px var(--color-inset-light)"
          : "none",
      }}
      onClick={() => p.onClick()}
      aria-current={p.active ? "page" : undefined}
      aria-label={label()}
      title={label()}
    >
      {/* active 左竖条(参考稿:2px 电青 bar) */}
      <Show when={p.active}>
        <span
          aria-hidden="true"
          style={{
            position: "absolute",
            left: p.expanded ? "-2px" : "0",
            top: "50%",
            transform: "translateY(-50%)",
            width: "2px",
            height: "20px",
            "border-radius": "0 2px 2px 0",
            background: "var(--color-accent-primary)",
          }}
        />
      </Show>
      <div
        class="flex items-center min-w-0"
        style={{ gap: p.expanded ? "12px" : "0" }}
      >
        <div
          style={{
            width: "20px",
            height: "20px",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            "flex-shrink": 0,
          }}
        >
          <Icon />
        </div>
        <Show when={p.expanded}>
          <span class="truncate" style={{ "font-size": "14px" }}>
            {label()}
          </span>
          <Show when={p.badge}>
            <span
              style={{
                "font-size": "10px",
                "font-weight": 700,
                color: "var(--color-accent-primary)",
                background: "var(--color-accent-soft)",
                "border-radius": "4px",
                padding: "1px 4px",
                "flex-shrink": 0,
                "margin-left": "4px",
              }}
            >
              {p.badge}
            </span>
          </Show>
        </Show>
      </div>
      <Show when={p.expanded}>
        <span
          style={{
            "font-size": "12px",
            color: "var(--color-text-tertiary)",
            "flex-shrink": 0,
          }}
        >
          {p.count}
        </span>
      </Show>
    </button>
  );
};

const divider = () => (
  <div
    style={{
      height: "1px",
      background: "var(--color-bg-hover)",
      margin: "4px 10px",
    }}
  />
);

const SectionLabel = (props: { children: string }) => (
  <div
    style={{
      "font-size": "11px",
      "font-weight": 600,
      color: "var(--color-text-tertiary)",
      "text-transform": "uppercase",
      "letter-spacing": "0.5px",
      padding: "0 8px",
      "margin-bottom": "4px",
    }}
  >
    {props.children}
  </div>
);

export default function Sidebar() {
  // 状态全部来自全局 store(Iteration 13 迁移)
  const width = () => $ui.sidebarWidth();
  const isPinned = () => $ui.sidebarPinned();
  const isCollapsed = () => $ui.sidebarCollapsed();

  // 响应式 + 动效偏好
  const isNarrow = useIsNarrowScreen();
  const reducedMotion = useReducedMotion();

  // 窄屏强制 collapsed + 取消 pin(不可展开,只能用轨道)
  const effectiveCollapsed = () => isNarrow() || isCollapsed();
  const effectivePinned = () => !isNarrow() && isPinned();

  let hoverTimer: number | null = null;

  const expand = () => {
    if (effectivePinned() || isNarrow()) return;
    if (hoverTimer) {
      clearTimeout(hoverTimer);
      hoverTimer = null;
    }
    $ui.setSidebarCollapsed(false);
  };

  const scheduleCollapse = () => {
    if (effectivePinned() || isNarrow()) return;
    if (hoverTimer) clearTimeout(hoverTimer);
    hoverTimer = window.setTimeout(
      () => $ui.setSidebarCollapsed(true),
      HOVER_COLLAPSE_DELAY,
    );
  };

  // 占位宽度:collapsed 仅留轨道;展开 = 面板宽度
  const placeholderWidth = () =>
    effectiveCollapsed() ? RAIL_WIDTH : width();

  const handleDragStart = (e: MouseEvent) => {
    if (isNarrow()) return; // 窄屏不可调宽
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = width();
    document.body.style.cursor = "col-resize";

    const handleMove = (ev: MouseEvent) => {
      const newWidth = Math.max(
        MIN_EXPANDED_WIDTH,
        Math.min(MAX_WIDTH, startWidth + ev.clientX - startX),
      );
      $ui.commitSidebarWidth(newWidth);
    };

    const handleUp = () => {
      document.body.style.cursor = "";
      document.removeEventListener("mousemove", handleMove);
      document.removeEventListener("mouseup", handleUp);
    };

    document.addEventListener("mousemove", handleMove);
    document.addEventListener("mouseup", handleUp);
  };

  const taskCounts = () => $taskFilter.taskCounts();
  const fileTypeCounts = () => $taskFilter.fileTypeCounts();
  const sidebarFilter = () => $taskFilter.sidebarFilter();
  const fileTypeFilter = () => $taskFilter.fileTypeFilter();

  // 展开面板固定宽度,transform 滑入滑出(合成层,零 reflow)
  // spec 8.3:必须用 translate3d(transform-gpu)而非 translateX,确保 GPU 合成
  const panelWidth = () => Math.max(width(), MIN_EXPANDED_WIDTH);
  const panelTranslateX = () =>
    effectiveCollapsed() ? `-${panelWidth()}px` : "0px";

  // 动效:reducedMotion 时即时,否则平滑过渡
  const transitionStyle = () =>
    reducedMotion() ? "none" : "transform 200ms cubic-bezier(0.32, 0.72, 0, 1)";
  const widthTransitionStyle = () =>
    reducedMotion() ? "none" : "width 220ms cubic-bezier(0.32, 0.72, 0, 1)";

  // 轨道图标列表(collapsed 态可见)
  const railEntries = createMemo(() => [
    ...statusItems.map((it) => ({
      key: `s-${it.key}`,
      labelKey: it.labelKey,
      icon: it.icon,
      active: sidebarFilter() === it.key,
      onClick: () => toggleSidebarFilter(it.key),
    })),
    ...typeItems.map((it) => ({
      key: `t-${it.key}`,
      labelKey: it.labelKey,
      icon: it.icon,
      active: fileTypeFilter() === it.key,
      onClick: () => toggleFileTypeFilter(it.key),
    })),
    {
      key: "sniffer",
      labelKey: "command.nav.sniffer.label" as MessageKey,
      icon: RadarIcon,
      active: false,
      onClick: $ui.openSniffer,
    },
    {
      key: "history",
      labelKey: "command.nav.history.label" as MessageKey,
      icon: HistoryIcon,
      active: false,
      onClick: $ui.openHistory,
    },
  ]);

  return (
    <>
      {/* Edge trigger zone — 仅 collapsed 且非 pinned 且非窄屏时 */}
      <Show when={!effectivePinned() && effectiveCollapsed() && !isNarrow()}>
        <div
          class="fixed left-0 top-0 bottom-0 z-[5]"
          style={{ width: `${EDGE_ZONE_WIDTH}px` }}
          onMouseEnter={expand}
        />
      </Show>

      {/* 占位容器:宽度过渡是唯一 reflow 源 */}
      <div
        class="relative flex-shrink-0 h-full overflow-hidden"
        style={{
          width: `${placeholderWidth()}px`,
          transition: widthTransitionStyle(),
          "will-change": "width",
        }}
        onMouseEnter={expand}
        onMouseLeave={scheduleCollapse}
      >
        {/* 常驻轨道:始终显示图标,collapsed 态可用 */}
        <div
          class="h-full flex flex-col"
          style={{
            width: `${RAIL_WIDTH}px`,
            background: "var(--color-bg-secondary)",
            "border-right": "1px solid var(--color-border-subtle)",
            position: "absolute",
            left: 0,
            top: 0,
            bottom: 0,
            padding: "6px 8px",
            gap: "2px",
            "z-index": "1",
          }}
        >
          <div
            class="flex items-center justify-center flex-shrink-0"
            style={{ height: "34px", "margin-bottom": "2px" }}
          >
            <Button
              variant="ghost"
              shape="icon-sm"
              aria-label={effectivePinned() ? tr("sidebar.aria.unpin") : tr("sidebar.aria.pin")}
              title={effectivePinned() ? tr("sidebar.aria.unpin") : tr("sidebar.aria.pin")}
              class={effectivePinned() ? "is-pinned" : ""}
              onClick={$ui.toggleSidebarPin}
              disabled={isNarrow()}
            >
              {effectivePinned() ? <PinIcon /> : <PinOffIcon />}
            </Button>
          </div>
          {divider()}
          <For each={railEntries()}>
            {(entry) => (
              <NavItem
                icon={entry.icon}
                labelKey={entry.labelKey}
                count={0}
                active={entry.active}
                expanded={false}
                onClick={entry.onClick}
              />
            )}
          </For>
          <div class="flex-1" />
        </div>

        {/* 展开面板:固定宽度,transform 滑入滑出 */}
        <div
          class="h-full flex flex-col"
          style={{
            width: `${panelWidth()}px`,
            /* 去 AI 味:实色底,移除顶部高光渐变与 inset 装饰 */
            background: "var(--color-bg-secondary)",
            "border-right": "1px solid var(--color-border-subtle)",
            transform: `translate3d(${panelTranslateX()}, 0, 0)`,
            transition: transitionStyle(),
            "will-change": "transform",
            position: "absolute",
            left: 0,
            top: 0,
            bottom: 0,
            "z-index": "2",
            "pointer-events": effectiveCollapsed() ? "none" : "auto",
            "box-shadow": effectiveCollapsed() ? "none" : "var(--shadow-md)",
          }}
        >
          {/* Pin header */}
          <div
            class="flex items-center justify-between flex-shrink-0"
            style={{
              height: "40px",
              padding: "0 12px",
              "border-bottom": "1px solid var(--color-border-subtle)",
            }}
          >
            <span
              style={{
                "font-size": "11px",
                "font-weight": 600,
                color: "var(--color-text-tertiary)",
                "letter-spacing": "0.5px",
              }}
            >
              {tr("sidebar.nav")}
            </span>
            <Button
              variant="ghost"
              shape="icon-sm"
              aria-label={effectivePinned() ? tr("sidebar.aria.unpin") : tr("sidebar.aria.pin")}
              title={effectivePinned() ? tr("sidebar.aria.unpin") : tr("sidebar.aria.pin")}
              class={effectivePinned() ? "is-pinned" : ""}
              onClick={$ui.toggleSidebarPin}
            >
              {effectivePinned() ? <PinIcon /> : <PinOffIcon />}
            </Button>
          </div>

          {/* Status section */}
          <div class="flex flex-col gap-1" style={{ padding: "8px 6px" }}>
            <SectionLabel>{tr("sidebar.section.status")}</SectionLabel>
            <For each={statusItems}>
              {(item) => (
                <NavItem
                  icon={item.icon}
                  labelKey={item.labelKey}
                  count={taskCounts()[item.key]}
                  active={sidebarFilter() === item.key}
                  expanded={true}
                  onClick={() => toggleSidebarFilter(item.key)}
                />
              )}
            </For>
          </div>

          {divider()}

          {/* Type section */}
          <div class="flex flex-col gap-1" style={{ padding: "8px 6px" }}>
            <SectionLabel>{tr("sidebar.section.type")}</SectionLabel>
            <For each={typeItems}>
              {(item) => (
                <NavItem
                  icon={item.icon}
                  labelKey={item.labelKey}
                  count={fileTypeCounts()[item.key]}
                  active={fileTypeFilter() === item.key}
                  expanded={true}
                  onClick={() => toggleFileTypeFilter(item.key)}
                />
              )}
            </For>
          </div>

          {divider()}

          {/* Lab section */}
          <div class="flex flex-col gap-1" style={{ padding: "8px 6px" }}>
            <SectionLabel>{tr("sidebar.section.lab")}</SectionLabel>
            <NavItem
              icon={RadarIcon}
              labelKey={"command.nav.sniffer.label"}
              count={0}
              active={false}
              expanded={true}
              onClick={$ui.openSniffer}
            />
            <NavItem
              icon={HistoryIcon}
              labelKey={"command.nav.history.label"}
              count={0}
              active={false}
              expanded={true}
              onClick={$ui.openHistory}
            />
            <Show when={$experimental.isEnabled("huggingface")}>
              <NavItem
                icon={HubIcon}
                labelKey={"sidebar.huggingface"}
                count={0}
                active={false}
                expanded={true}
                badge="β"
                onClick={$ui.openHub}
              />
            </Show>
          </div>

          <div class="flex-1" />

          {/* Resize handle */}
          <div
            class="resize-handle absolute right-0 top-0 bottom-0 cursor-col-resize z-10"
            style={{
              width: "4px",
              background: "transparent",
              transition: "background 150ms ease",
            }}
            onMouseDown={handleDragStart}
          />
        </div>
      </div>
    </>
  );
}
