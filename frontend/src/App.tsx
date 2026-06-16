import { createSignal, Show, lazy, Suspense, ErrorBoundary } from "solid-js";
import type { ListDensity, SnifferResource, TaskInfo, ViewName } from "./types";
import { api } from "./api/invoke";
import {
  $activeCount,
  $tasks,
  $selectedId,
  $selectedTask,
  $totalSpeed,
  refreshTaskList,
} from "./stores/downloads";
import { addToast } from "./stores/toast";
import {
  $selectedIds,
  deselectAll,
  selectAll,
  toggleSelection,
} from "./stores/selection";
import {
  $ui,
  openView,
  openNewTaskModal,
  closeShortcutHelp,
} from "./stores/ui";
import {
  $taskFilter,
  setSearchQuery,
  removeSearchFilter,
} from "./stores/taskFilter";
import {
  pauseSelected,
  resumeSelected,
  deleteSelected,
  pauseAll,
  resumeAll,
} from "./stores/batchActions";
import { useAppInit } from "./hooks/useAppInit";
import { useGlobalKeyboard } from "./hooks/useGlobalKeyboard";
import { useContextMenu } from "./hooks/useContextMenu";
import { useDragDrop } from "./hooks/useDragDrop";
import TitleBar from "./components/TitleBar";
import Sidebar from "./components/Sidebar";
import Toolbar from "./components/Toolbar";
import TaskList from "./components/TaskList";
import DetailPanel from "./components/DetailPanel";
import StatusBar from "./components/StatusBar";
import ToastContainer from "./components/ToastContainer";
import ContextMenu from "./components/ContextMenu";
import BatchToolbar from "./components/BatchToolbar";

const SnifferPanel = lazy(() => import("./components/SnifferPanel"));
const HistoryPanel = lazy(() => import("./components/HistoryPanel"));
const SettingsPanel = lazy(() => import("./components/SettingsPanel"));
const CommandPalette = lazy(() => import("./components/CommandPalette"));
const NewTaskModal = lazy(() => import("./components/NewTaskModal"));
const HfBrowserPanel = lazy(() => import("./components/HfBrowserPanel"));
const ShortcutHelp = lazy(() => import("./components/ShortcutHelp"));

function AppContent() {
  const [listDensity, setListDensity] =
    createSignal<ListDensity>("comfortable");
  const [isMultiSelectMode, setIsMultiSelectMode] = createSignal(false);
  const [snifferResources, setSnifferResources] = createSignal<
    SnifferResource[]
  >([]);

  useAppInit(setSnifferResources);
  useGlobalKeyboard();
  const {
    contextMenu,
    open: openContextMenu,
    close: closeContextMenu,
  } = useContextMenu();
  const { isDragOver, handleDragOver, handleDragLeave, handleDrop } =
    useDragDrop();

  const handleDetailClose = () => {
    $selectedId.set(null);
  };

  const handleTaskClick = (taskId: string) => {
    if (isMultiSelectMode()) {
      toggleSelection(taskId);
    } else {
      $selectedId.set((prev) => (prev === taskId ? null : taskId));
    }
  };

  const handleSelectAll = () => {
    const allIds = $taskFilter.filteredTasks().map((t) => t.id);
    const current = $selectedIds.get();
    if (current.size === allIds.length) {
      deselectAll();
    } else {
      selectAll(allIds);
    }
  };

  const handleViewChange = (view: ViewName) => {
    openView(view);
  };

  const handleAddFromSniffer = (resource: SnifferResource) => {
    api
      .createTask(resource.url)
      .then(() => refreshTaskList())
      .catch((e) => addToast(`创建任务失败: ${e}`, "error"));
  };

  const handleRedownload = (task: TaskInfo) => {
    api
      .createTask(task.url)
      .then(() => refreshTaskList())
      .catch((e) => addToast(`重新下载失败: ${e}`, "error"));
  };

  const handleDeleteRecord = (taskId: string) => {
    api
      .deleteTask(taskId)
      .then(() => refreshTaskList())
      .catch((e) => addToast(`删除记录失败: ${e}`, "error"));
  };

  return (
    <div
      class="w-screen h-screen flex flex-col overflow-hidden"
      style={{ background: "var(--color-bg-primary)" }}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {/* Drag overlay */}
      <Show when={isDragOver()}>
        <div
          class="fixed inset-0 z-[300] flex items-center justify-center"
          style={{
            background: "var(--color-overlay-scrim)",
            "backdrop-filter": "blur(4px)",
            animation: "fadeIn 150ms ease forwards",
          }}
        >
          <div
            class="flex flex-col items-center gap-4"
            style={{
              padding: "48px 64px",
              "border-radius": "16px",
              border: "2px dashed var(--color-accent-primary)",
              background: "var(--color-accent-soft)",
            }}
          >
            <svg
              width="48"
              height="48"
              viewBox="0 0 24 24"
              fill="none"
              stroke="var(--color-accent-primary)"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
              <polyline points="17 8 12 3 7 8" />
              <line x1="12" y1="3" x2="12" y2="15" />
            </svg>
            <span
              style={{
                "font-size": "16px",
                color: "var(--color-accent-primary)",
                "font-weight": 500,
              }}
            >
              拖放链接到此处开始下载
            </span>
          </div>
        </div>
      </Show>

      <TitleBar />

      <div class="flex flex-1 overflow-hidden">
        <Sidebar />

        <div class="flex-1 flex flex-col min-w-0">
          <Toolbar
            searchQuery={$taskFilter.searchQuery()}
            onSearchChange={setSearchQuery}
            filters={$taskFilter.searchFilters().filters}
            onRemoveFilter={removeSearchFilter}
            isMultiSelectMode={isMultiSelectMode()}
            onToggleMultiSelect={() => {
              setIsMultiSelectMode((v) => !v);
              deselectAll();
            }}
            selectedCount={$selectedIds.get().size}
            onSelectAll={handleSelectAll}
            onPauseSelected={pauseSelected}
            onResumeSelected={resumeSelected}
            onDeleteSelected={deleteSelected}
            onExitMultiSelect={() => {
              setIsMultiSelectMode(false);
              deselectAll();
            }}
            listDensity={listDensity()}
            onToggleDensity={() =>
              setListDensity((d) =>
                d === "comfortable" ? "compact" : "comfortable",
              )
            }
            onNewTask={openNewTaskModal}
            onOpenSettings={$ui.openSettings}
            onPauseAll={pauseAll}
            onResumeAll={resumeAll}
          />

          <div class="flex flex-1 overflow-hidden">
            <TaskList
              tasks={$taskFilter.filteredTasks()}
              selectedTaskId={$selectedId.get()}
              onTaskClick={handleTaskClick}
              onTaskContextMenu={openContextMenu}
              isMultiSelectMode={isMultiSelectMode()}
              selectedTaskIds={$selectedIds.get()}
              density={listDensity()}
              searchQuery={$taskFilter.searchQuery()}
              onNewTask={openNewTaskModal}
            />

            <DetailPanel
              task={$selectedTask.get()}
              onClose={handleDetailClose}
            />
          </div>
        </div>
      </div>

      <StatusBar
        isIdle={$activeCount.get() === 0}
        totalSpeed={$totalSpeed.get()}
        activeCount={$activeCount.get()}
        pausedCount={$taskFilter.taskCounts().paused}
        totalCount={$tasks.get().length}
      />

      <ToastContainer />

      <Show when={$ui.newTaskModalOpen()}>
        <Suspense
          fallback={<div class="animate-pulse bg-white/5 rounded-lg h-full" />}
        >
          <NewTaskModal onClose={$ui.closeNewTaskModal} />
        </Suspense>
      </Show>

      {/* Context Menu */}
      <ContextMenu
        x={contextMenu().x}
        y={contextMenu().y}
        visible={contextMenu().visible}
        task={contextMenu().task}
        onClose={closeContextMenu}
        onPause={(taskId) => {
          api
            .pauseTask(taskId)
            .then(() => refreshTaskList())
            .catch((e) => addToast(`暂停失败: ${e}`, "error"));
        }}
        onResume={(taskId) => {
          api
            .resumeTask(taskId)
            .then(() => refreshTaskList())
            .catch((e) => addToast(`恢复失败: ${e}`, "error"));
        }}
        onOpenFolder={(taskId) => {
          const task = $tasks.get().find((t) => t.id === taskId);
          if (task?.savePath) {
            api.openFolder(task.savePath).catch(() => {
              addToast("打开文件夹失败", "error");
            });
          } else {
            addToast("该任务暂无保存路径信息", "info");
          }
        }}
        onCopyLink={(taskId) => {
          const task = $tasks.get().find((t) => t.id === taskId);
          if (task) navigator.clipboard.writeText(task.url);
        }}
        onRedownload={(taskId) => {
          const task = $tasks.get().find((t) => t.id === taskId);
          if (task) handleRedownload(task);
        }}
        onDelete={(taskId) => {
          api
            .deleteTask(taskId)
            .then(() => refreshTaskList())
            .catch((e) => addToast(`删除失败: ${e}`, "error"));
          if ($selectedId.get() === taskId) $selectedId.set(null);
        }}
      />

      {/* Panels */}
      <Show when={$ui.snifferVisible()}>
        <Suspense
          fallback={<div class="animate-pulse bg-white/5 rounded-lg h-full" />}
        >
          <SnifferPanel
            visible={$ui.snifferVisible()}
            resources={snifferResources()}
            onClose={$ui.closeSniffer}
            onAddDownload={handleAddFromSniffer}
          />
        </Suspense>
      </Show>

      <Show when={$ui.historyVisible()}>
        <Suspense
          fallback={<div class="animate-pulse bg-white/5 rounded-lg h-full" />}
        >
          <HistoryPanel
            visible={$ui.historyVisible()}
            tasks={$tasks.get()}
            onClose={$ui.closeHistory}
            onOpenFolder={(taskId) => {
              const task = $tasks.get().find((t) => t.id === taskId);
              if (task?.savePath) {
                api.openFolder(task.savePath).catch(() => {
                  addToast("打开文件夹失败", "error");
                });
              } else {
                addToast("该记录暂无保存路径信息", "info");
              }
            }}
            onRedownload={handleRedownload}
            onDeleteRecord={handleDeleteRecord}
          />
        </Suspense>
      </Show>

      <Show when={$ui.settingsVisible()}>
        <Suspense
          fallback={<div class="animate-pulse bg-white/5 rounded-lg h-full" />}
        >
          <SettingsPanel
            visible={$ui.settingsVisible()}
            onClose={$ui.closeSettings}
          />
        </Suspense>
      </Show>

      <Show when={$ui.hubVisible()}>
        <Suspense
          fallback={<div class="animate-pulse bg-white/5 rounded-lg h-full" />}
        >
          <HfBrowserPanel visible={$ui.hubVisible()} onClose={$ui.closeHub} />
        </Suspense>
      </Show>

      <Show when={$ui.commandPaletteOpen()}>
        <Suspense
          fallback={<div class="animate-pulse bg-white/5 rounded-lg h-full" />}
        >
          <CommandPalette
            open={$ui.commandPaletteOpen()}
            onClose={$ui.closeCommandPalette}
            onViewChange={handleViewChange}
            onNewDownload={openNewTaskModal}
            onPauseAll={pauseAll}
            onResumeAll={resumeAll}
          />
        </Suspense>
      </Show>

      <Show when={$ui.shortcutHelpOpen()}>
        <Suspense
          fallback={<div class="animate-pulse bg-white/5 rounded-lg h-full" />}
        >
          <ShortcutHelp
            visible={$ui.shortcutHelpOpen()}
            onClose={closeShortcutHelp}
          />
        </Suspense>
      </Show>

      <BatchToolbar
        onPauseAll={pauseSelected}
        onResumeAll={resumeSelected}
        onDeleteAll={deleteSelected}
      />
    </div>
  );
}

export default function App() {
  return (
    <ErrorBoundary
      fallback={(err) => (
        <div
          class="flex items-center justify-center p-8"
          style={{
            "min-height": "100dvh",
            background: "var(--color-bg-primary)",
            color: "var(--color-text-primary)",
          }}
        >
          <div
            class="panel-surface rounded-lg p-6 max-w-md"
            style={{ "box-shadow": "var(--shadow-lg)" }}
          >
            <div
              style={{
                "font-size": "16px",
                "font-weight": 600,
                color: "var(--color-error)",
                "margin-bottom": "8px",
              }}
            >
              应用发生错误
            </div>
            <div
              class="mono"
              style={{
                "font-size": "13px",
                color: "var(--color-text-secondary)",
                "word-break": "break-all",
              }}
            >
              {String(err)}
            </div>
          </div>
        </div>
      )}
    >
      <AppContent />
    </ErrorBoundary>
  );
}
