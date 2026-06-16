import { createSignal, createRoot, type Accessor } from "solid-js";
import type { ViewName } from "../types";

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
