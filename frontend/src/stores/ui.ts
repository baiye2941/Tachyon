import { createSignal, createRoot, type Accessor } from "solid-js";
import type { ViewName } from "../types";

// 侧边栏常量(与 Sidebar.tsx 保持一致,单一来源)
export const SIDEBAR_RAIL_WIDTH = 56;
export const SIDEBAR_MIN_EXPANDED_WIDTH = 200;
export const SIDEBAR_MAX_WIDTH = 300;
export const SIDEBAR_DEFAULT_WIDTH = 220;
const SIDEBAR_STORAGE_KEY = "tachyon-sidebar-state";

type PanelState = {
  snifferVisible: boolean;
  historyVisible: boolean;
  settingsVisible: boolean;
  hubVisible: boolean;
  newTaskModalOpen: boolean;
  commandPaletteOpen: boolean;
  shortcutHelpOpen: boolean;
};

const [snifferVisible, setSnifferVisible] = createSignal(false);
const [historyVisible, setHistoryVisible] = createSignal(false);
const [settingsVisible, setSettingsVisible] = createSignal(false);
const [hubVisible, setHubVisible] = createSignal(false);
const [newTaskModalOpen, setNewTaskModalOpen] = createSignal(false);
const [commandPaletteOpen, setCommandPaletteOpen] = createSignal(false);
const [shortcutHelpOpen, setShortcutHelpOpen] = createSignal(false);

// —— 侧边栏状态(Iteration 13 从 Sidebar.tsx 本地信号迁移到全局 store)——
// 迁移动机:使命令面板/快捷键能控制侧边栏伸缩,消除"状态散落在组件内"的技术债。

interface SidebarPersistedState {
  width: number;
  pinned: boolean;
}

function loadSidebarState(): SidebarPersistedState {
  try {
    const raw = localStorage.getItem(SIDEBAR_STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<SidebarPersistedState>;
      const width =
        typeof parsed.width === "number" &&
        parsed.width >= SIDEBAR_MIN_EXPANDED_WIDTH &&
        parsed.width <= SIDEBAR_MAX_WIDTH
          ? parsed.width
          : SIDEBAR_DEFAULT_WIDTH;
      return { width, pinned: Boolean(parsed.pinned) };
    }
  } catch {
    /* ignore */
  }
  return { width: SIDEBAR_DEFAULT_WIDTH, pinned: false };
}

function saveSidebarState(width: number, pinned: boolean): void {
  try {
    localStorage.setItem(
      SIDEBAR_STORAGE_KEY,
      JSON.stringify({ width, pinned }),
    );
  } catch {
    /* ignore */
  }
}

const initialSidebar = loadSidebarState();
const [sidebarWidth, setSidebarWidth] = createSignal(initialSidebar.width);
const [sidebarPinned, setSidebarPinned] = createSignal(initialSidebar.pinned);
// collapsed:pinned 时永远 false;非 pinned 时由 hover 或外部动作控制
const [sidebarCollapsed, setSidebarCollapsed] = createSignal(
  !initialSidebar.pinned,
);

function toggleSidebarPin(): void {
  const next = !sidebarPinned();
  setSidebarPinned(next);
  setSidebarCollapsed(!next);
  if (next && sidebarWidth() < SIDEBAR_MIN_EXPANDED_WIDTH) {
    setSidebarWidth(SIDEBAR_MIN_EXPANDED_WIDTH);
  }
  saveSidebarState(sidebarWidth(), next);
}

function toggleSidebar(): void {
  // pinned 时 toggle 收起为 collapsed;collapsed 时 toggle 展开(临时,非 pin)
  if (sidebarPinned()) {
    setSidebarPinned(false);
    setSidebarCollapsed(true);
    saveSidebarState(sidebarWidth(), false);
  } else {
    setSidebarCollapsed((v) => !v);
  }
}

function setSidebarCollapsedState(collapsed: boolean): void {
  setSidebarCollapsed(collapsed);
}

function commitSidebarWidth(w: number): void {
  setSidebarWidth(w);
  saveSidebarState(w, sidebarPinned());
}

// 顶层导出:供 useGlobalKeyboard 的 Ctrl+B 快捷键直接调用(Iteration 13)
export { toggleSidebar, toggleSidebarPin };

function closeAllPanels(): void {
  setSnifferVisible(false);
  setHistoryVisible(false);
  setSettingsVisible(false);
  setHubVisible(false);
}

export function openSniffer(): void {
  closeAllPanels();
  setSnifferVisible(true);
}

export function closeSniffer(): void {
  setSnifferVisible(false);
}

export function toggleSniffer(): void {
  setSnifferVisible((v) => !v);
}

export function openHistory(): void {
  closeAllPanels();
  setHistoryVisible(true);
}

export function closeHistory(): void {
  setHistoryVisible(false);
}

export function toggleHistory(): void {
  setHistoryVisible((v) => !v);
}

export function openSettings(): void {
  closeAllPanels();
  setSettingsVisible(true);
}

export function closeSettings(): void {
  setSettingsVisible(false);
}

export function toggleSettings(): void {
  setSettingsVisible((v) => !v);
}

export function openHub(): void {
  closeAllPanels();
  setHubVisible(true);
}

export function closeHub(): void {
  setHubVisible(false);
}

export function toggleHub(): void {
  setHubVisible((v) => !v);
}

export function openNewTaskModal(): void {
  setNewTaskModalOpen(true);
}

export function closeNewTaskModal(): void {
  setNewTaskModalOpen(false);
}

export function openCommandPalette(): void {
  setCommandPaletteOpen(true);
}

export function closeCommandPalette(): void {
  setCommandPaletteOpen(false);
}

export function toggleCommandPalette(): void {
  setCommandPaletteOpen((v) => !v);
}

export function openShortcutHelp(): void {
  setShortcutHelpOpen(true);
}

export function closeShortcutHelp(): void {
  setShortcutHelpOpen(false);
}

export function toggleShortcutHelp(): void {
  setShortcutHelpOpen((v) => !v);
}

export function openView(view: ViewName): void {
  closeAllPanels();
  closeCommandPalette();

  if (view === "sniffer") {
    setSnifferVisible(true);
  } else if (view === "history") {
    setHistoryVisible(true);
  } else if (view === "settings") {
    setSettingsVisible(true);
  } else if (view === "hub") {
    setHubVisible(true);
  }
}

export function closeView(view: ViewName | "command"): void {
  if (view === "sniffer") {
    setSnifferVisible(false);
  } else if (view === "history") {
    setHistoryVisible(false);
  } else if (view === "settings") {
    setSettingsVisible(false);
  } else if (view === "hub") {
    setHubVisible(false);
  } else if (view === "command") {
    closeCommandPalette();
  }
}

export const $ui = {
  get snifferVisible(): Accessor<boolean> {
    return snifferVisible;
  },
  get historyVisible(): Accessor<boolean> {
    return historyVisible;
  },
  get settingsVisible(): Accessor<boolean> {
    return settingsVisible;
  },
  get hubVisible(): Accessor<boolean> {
    return hubVisible;
  },
  get newTaskModalOpen(): Accessor<boolean> {
    return newTaskModalOpen;
  },
  get commandPaletteOpen(): Accessor<boolean> {
    return commandPaletteOpen;
  },
  get shortcutHelpOpen(): Accessor<boolean> {
    return shortcutHelpOpen;
  },
  // —— 侧边栏(Iteration 13)——
  get sidebarWidth(): Accessor<number> {
    return sidebarWidth;
  },
  get sidebarPinned(): Accessor<boolean> {
    return sidebarPinned;
  },
  get sidebarCollapsed(): Accessor<boolean> {
    return sidebarCollapsed;
  },
  toggleSidebarPin,
  toggleSidebar,
  setSidebarCollapsed: setSidebarCollapsedState,
  commitSidebarWidth,
  openSniffer,
  closeSniffer,
  toggleSniffer,
  openHistory,
  closeHistory,
  toggleHistory,
  openSettings,
  closeSettings,
  toggleSettings,
  openHub,
  closeHub,
  toggleHub,
  openNewTaskModal,
  closeNewTaskModal,
  openCommandPalette,
  closeCommandPalette,
  toggleCommandPalette,
  openShortcutHelp,
  closeShortcutHelp,
  toggleShortcutHelp,
  openView,
  closeView,
};

// 在 createRoot 下读取状态，避免测试环境出现 computations created outside a createRoot 警告
export function readUiState(): PanelState {
  return createRoot((dispose) => {
    try {
      return {
        snifferVisible: snifferVisible(),
        historyVisible: historyVisible(),
        settingsVisible: settingsVisible(),
        hubVisible: hubVisible(),
        newTaskModalOpen: newTaskModalOpen(),
        commandPaletteOpen: commandPaletteOpen(),
        shortcutHelpOpen: shortcutHelpOpen(),
      };
    } finally {
      dispose();
    }
  });
}
