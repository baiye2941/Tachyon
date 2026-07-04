import { errorMessage } from "./utils/appError";
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
import { deleteHistoryRecord, getRecordById } from "./stores/history";
import {
  pauseSelected,
  resumeSelected,
  deleteSelected,
  cancelSelected,
  pauseAll,
  resumeAll,
  cancelAll,
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
import ConfirmDialog from "./components/ConfirmDialog";
import ErrorPage from "./components/ErrorPage";
import { $confirm, requestConfirm, resolveConfirm } from "./stores/confirm";
import { clearTaskHistory } from "./stores/taskSpeedHistory";
import { tr } from "./i18n";

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
      .createTask(resource.downloadUrl)
      .then(() => refreshTaskList())
      .catch((e) =>
        addToast(tr("toast.createTaskFailed", { error: errorMessage(e) }), "error"),
      );
  };

  const handleRedownload = (task: TaskInfo) => {
    api
      .createTask(task.url)
      .then(() => refreshTaskList())
      .catch((e) =>
        addToast(tr("toast.redownloadFailed", { error: errorMessage(e) }), "error"),
      );
  };

  const handleDeleteRecord = async (taskId: string) => {
    // 历史记录删除同样走应用层 ConfirmDialog(Iteration 11)
    const task = $tasks.get().find((t) => t.id === taskId);
    const record = getRecordById(taskId);
    const fileName = task?.fileName ?? record?.fileName ?? taskId;
    const result = await requestConfirm({
      title: tr("confirm.deleteHistory.title"),
      message: tr("confirm.deleteHistory.message", { name: fileName }),
      confirmLabel: tr("confirm.deleteHistory.confirmLabel"),
      tone: "danger",
      showDeleteLocalFileOption: true,
      deleteLocalFileDefault: false,
    });
    if (!result.ok) return;

    // 先移除本地历史记录与速度采样,保证 UI 即时响应;后端删除失败也不影响本地清理
    deleteHistoryRecord(taskId);
    clearTaskHistory(taskId);

    if (task) {
      try {
        await api.deleteTask(taskId, {
          skipConfirm: true,
          deleteLocalFile: result.deleteLocalFile,
        });
      } catch (e) {
        addToast(tr("toast.deleteRecordFailed", { error: errorMessage(e) }), "error");
      }
    }
    await refreshTaskList();
  };

  const handleDelete = async (taskId: string) => {
    // Iteration 11:走应用层 ConfirmDialog,与品牌视觉一致;
    // 后端 confirmation token 仍在 invoke 层执行,安全边界不变。
    const task = $tasks.get().find((t) => t.id === taskId);
    const fileName = task?.fileName ?? taskId;
    const result = await requestConfirm({
      title: tr("confirm.delete.title"),
      message: tr("confirm.delete.message", { name: fileName }),
      confirmLabel: tr("confirm.delete.confirmLabel"),
      showDeleteLocalFileOption: true,
      deleteLocalFileDefault: false,
      tone: "danger",
    });
    if (!result.ok) return;
    try {
      await api.deleteTask(taskId, {
        skipConfirm: true,
        deleteLocalFile: result.deleteLocalFile,
      });
      clearTaskHistory(taskId);
      await refreshTaskList();
      if ($selectedId.get() === taskId) $selectedId.set(null);
    } catch (e) {
      addToast(tr("toast.deleteFailed", { error: errorMessage(e) }), "error");
    }
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
              class="empty-state-icon"
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
              {tr("dragdrop.hint")}
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
              // 覆盖式详情:进入多选时清详情,否则详情遮罩盖住选中态(R1 回归修复)
              $selectedId.set(null);
            }}
            selectedCount={$selectedIds.get().size}
            onSelectAll={handleSelectAll}
            onPauseSelected={pauseSelected}
            onResumeSelected={resumeSelected}
            onCancelSelected={cancelSelected}
            onDeleteSelected={deleteSelected}
            onExitMultiSelect={() => {
              setIsMultiSelectMode(false);
              deselectAll();
              $selectedId.set(null);
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
            onCancelAll={cancelAll}
          />

          <div class="flex flex-1 overflow-hidden relative">
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

      {/* 全局应用内确认对话框(Iteration 11):
          所有破坏性操作(单/批量删除)走 requestConfirm,统一品牌视觉,
          替代 OS 原生 window.confirm / Tauri plugin-dialog。 */}
      <ConfirmDialog
        open={$confirm.pending() !== null}
        title={$confirm.pending()?.title ?? ""}
        message={$confirm.pending()?.message ?? ""}
        confirmLabel={$confirm.pending()?.confirmLabel}
        cancelLabel={$confirm.pending()?.cancelLabel}
        tone={$confirm.pending()?.tone}
        showDeleteLocalFileOption={
          $confirm.pending()?.showDeleteLocalFileOption
        }
        deleteLocalFileLabel={$confirm.pending()?.deleteLocalFileLabel}
        deleteLocalFileDescription={
          $confirm.pending()?.deleteLocalFileDescription
        }
        deleteLocalFileDefault={$confirm.pending()?.deleteLocalFileDefault}
        onConfirm={(options) => resolveConfirm({ ok: true, ...options })}
        onCancel={() => resolveConfirm(false)}
      />

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
            .catch((e) =>
              addToast(tr("toast.pauseFailed", { error: errorMessage(e) }), "error"),
            );
        }}
        onResume={(taskId) => {
          api
            .resumeTask(taskId)
            .then(() => refreshTaskList())
            .catch((e) =>
              addToast(tr("toast.resumeFailed", { error: errorMessage(e) }), "error"),
            );
        }}
        onCancel={(taskId) => {
          api
            .cancelTask(taskId)
            .then(() => refreshTaskList())
            .catch((e) =>
              addToast(tr("toast.cancelFailed", { error: errorMessage(e) }), "error"),
            );
        }}
        onOpenFolder={(taskId) => {
          const task = $tasks.get().find((t) => t.id === taskId);
          if (task?.savePath) {
            api.openFolder(task.savePath).catch(() => {
              addToast(tr("toast.openFolderFailed"), "error");
            });
          } else {
            addToast(tr("toast.noSavePath"), "info");
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
        onDelete={handleDelete}
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
            onOpenFolder={(savePath) => {
              // 问题2修复:直接用历史记录的 savePath 打开,
              // 不再按 id 查 $tasks(历史记录 id 与任务 id 不同,且任务可能已删除)
              if (savePath) {
                api.openFolder(savePath).catch(() => {
                  addToast(tr("toast.openFolderFailed"), "error");
                });
              } else {
                addToast(tr("toast.noSavePathRecord"), "info");
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
            initialTab={$ui.settingsInitialTab() ?? undefined}
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
            onToggleSidebar={$ui.toggleSidebar}
            getTasks={() => $tasks.get()}
            onOpenTask={(taskId) => $selectedId.set(taskId)}
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
    <ErrorBoundary fallback={(err) => <ErrorPage error={err} />}>
      <AppContent />
    </ErrorBoundary>
  );
}
