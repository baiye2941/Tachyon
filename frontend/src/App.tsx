import { errorMessage } from "./utils/appError";
import { createSignal, createEffect, Show, lazy, Suspense, ErrorBoundary } from "solid-js";
import type { SnifferResource, TaskInfo, ViewName, CaptureConfig } from "./types";
import { getParentDirectory } from "./utils/path";
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
  selectRange,
  toggleSelection,
  getLastSelectedAnchorId,
  setLastSelectedAnchorId,
  hasSelection,
  intersectSelection,
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
import { $taskListView, toggleGroupBy } from "./stores/taskListView";
import { $listDensity, toggleListDensity } from "./stores/listDensity";
import { deleteHistoryRecord, getRecordById } from "./stores/history";
import {
  pauseSelected,
  resumeSelected,
  deleteSelected,
  cancelSelected,
  pauseAll,
  resumeAll,
  cancelAll,
  clearCompleted,
  openSelectedFolders,
  copySelectedLinks,
  redownloadSelected,
} from "./stores/batchActions";
import { useAppInit } from "./hooks/useAppInit";
import { useGlobalKeyboard } from "./hooks/useGlobalKeyboard";
import { useIsWideScreen } from "./hooks/useMediaQuery";
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
import ConfirmDialog from "./components/ConfirmDialog";
import ErrorPage from "./components/ErrorPage";
import Skeleton from "./shared/ui/Skeleton";
import { $confirm, requestConfirm, resolveConfirm } from "./stores/confirm";
import { clearTaskHistory } from "./stores/taskSpeedHistory";
import { tr } from "./i18n";

const SnifferPanel = lazy(() => import("./components/SnifferPanel"));
const HistoryPanel = lazy(() => import("./components/HistoryPanel"));
const SettingsPanel = lazy(() => import("./components/settings/SettingsPanel"));
const CommandPalette = lazy(() => import("./components/CommandPalette"));
const NewTaskModal = lazy(() => import("./components/NewTaskModal"));
const HfBrowserPanel = lazy(() => import("./components/HfBrowserPanel"));
const ShortcutHelp = lazy(() => import("./components/ShortcutHelp"));

function AppContent() {
  const [isMultiSelectMode, setIsMultiSelectMode] = createSignal(false);
  const [snifferResources, setSnifferResources] = createSignal<
    SnifferResource[]
  >([]);
  const [snifferConfig, setSnifferConfig] = createSignal<CaptureConfig | null>(
    null,
  );

  useAppInit(setSnifferResources, (resource) =>
    setSnifferResources((prev) =>
      prev.some((r) => r.id === resource.id) ? prev : [resource, ...prev],
    ),
  );
  useGlobalKeyboard();
  const isWideScreen = useIsWideScreen();
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

  const handleTaskClick = (
    taskId: string,
    _index: number,
    shiftKey: boolean,
    orderedTaskIds: string[],
  ) => {
    if (isMultiSelectMode()) {
      const anchorId = getLastSelectedAnchorId();
      if (shiftKey && anchorId) {
        selectRange(anchorId, taskId, orderedTaskIds);
      } else {
        toggleSelection(taskId);
      }
      setLastSelectedAnchorId(taskId);
    } else {
      // 非多选模式下 Shift 点击临时开启一段范围选择,以最后一次单选为锚点
      const anchorId = $selectedId.get();
      if (shiftKey && anchorId) {
        setIsMultiSelectMode(true);
        selectRange(anchorId, taskId, orderedTaskIds);
      } else {
        $selectedId.set((prev) => (prev === taskId ? null : taskId));
      }
    }
  };

  const handleTaskActivate = (taskId: string, _index: number) => {
    if (isMultiSelectMode()) {
      toggleSelection(taskId);
      setLastSelectedAnchorId(taskId);
    } else {
      $selectedId.set(taskId);
    }
  };

  const handleSelectRange = (
    anchorIndex: number,
    endIndex: number,
    orderedTaskIds: string[],
  ) => {
    const anchorId = orderedTaskIds[anchorIndex];
    const endId = orderedTaskIds[endIndex];
    if (!anchorId || !endId) return;
    if (!isMultiSelectMode()) {
      setIsMultiSelectMode(true);
    }
    selectRange(anchorId, endId, orderedTaskIds);
    setLastSelectedAnchorId(anchorId);
  };

  const handleDeleteSelected = () => {
    if (!hasSelection()) return;
    deleteSelected();
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

  const handleClearSelection = () => {
    deselectAll();
  };

  // 选择集与过滤列表同步:过滤条件变化后,自动移除已被隐藏的已选任务,
  // 保证 "已选 N 项" 与批量操作范围始终对应用户当前可见的任务。
  createEffect(() => {
    const filteredIds = new Set($taskFilter.filteredTasks().map((t) => t.id));
    intersectSelection(filteredIds);
  });

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

  const handleAddSnifferResource = (url: string) => {
    api
      .addSnifferResource(url)
      .catch((e) =>
        addToast(tr("toast.snifferAddFailed", { error: errorMessage(e) }), "error"),
      );
  };

  const handleClearSnifferResources = () => {
    api
      .clearSnifferResources()
      .then(() => setSnifferResources([]))
      .catch((e) =>
        addToast(tr("toast.snifferClearFailed", { error: errorMessage(e) }), "error"),
      );
  };

  const handleUpdateSnifferConfig = (config: CaptureConfig) => {
    // 乐观更新:先更新本地状态,再异步提交后端
    setSnifferConfig(config);
    api
      .setSnifferCaptureConfig(config)
      .catch((e) =>
        addToast(
          tr("sniffer.config.updateFailed", { error: errorMessage(e) }),
          "error",
        ),
      );
  };

  // 嗅探面板首次打开时加载捕获配置(响应式:监听面板可见性)
  createEffect(() => {
    if (!$ui.snifferVisible() || snifferConfig()) return;
    api
      .getSnifferCaptureConfig()
      .then(setSnifferConfig)
      .catch((e) =>
        addToast(
          tr("sniffer.config.loadFailed", { error: errorMessage(e) }),
          "error",
        ),
      );
  });

  const handleRedownload = (task: TaskInfo) => {
    api
      .createTask(task.url)
      .then(() => refreshTaskList())
      .catch((e) =>
        addToast(tr("toast.redownloadFailed", { error: errorMessage(e) }), "error"),
      );
  };

  const handleDeleteRecord = async (
    taskId: string,
    opts?: { skipConfirm?: boolean; deleteLocalFile?: boolean },
  ) => {
    // 历史记录删除同样走应用层 ConfirmDialog(Iteration 11)
    const task = $tasks.get().find((t) => t.id === taskId);
    const record = getRecordById(taskId);
    const fileName = task?.fileName ?? record?.fileName ?? taskId;

    let deleteLocalFile = opts?.deleteLocalFile ?? false;
    if (!opts?.skipConfirm) {
      const result = await requestConfirm({
        title: tr("confirm.deleteHistory.title"),
        message: tr("confirm.deleteHistory.message", { name: fileName }),
        confirmLabel: tr("confirm.deleteHistory.confirmLabel"),
        tone: "danger",
        showDeleteLocalFileOption: true,
        deleteLocalFileDefault: false,
      });
      if (!result.ok) return;
      deleteLocalFile = result.deleteLocalFile;
    }

    // 先移除本地历史记录与速度采样,保证 UI 即时响应;后端删除失败也不影响本地清理
    deleteHistoryRecord(taskId);
    clearTaskHistory(taskId);

    if (task) {
      try {
        await api.deleteTask(taskId, {
          skipConfirm: true,
          deleteLocalFile,
        });
      } catch (e) {
        addToast(
          tr("toast.deleteRecordFailed", { error: errorMessage(e) }),
          "error",
        );
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
          class="fixed inset-0 z-[var(--z-drag)] flex items-center justify-center"
          style={{
            background: "var(--color-overlay-scrim)",
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
            totalCount={$taskFilter.filteredTasks().length}
            onSelectAll={handleSelectAll}
            onPauseSelected={pauseSelected}
            onResumeSelected={resumeSelected}
            onCancelSelected={cancelSelected}
            onDeleteSelected={deleteSelected}
            onOpenSelectedFolders={openSelectedFolders}
            onCopySelectedLinks={copySelectedLinks}
            onRedownloadSelected={redownloadSelected}
            onClearSelection={handleClearSelection}
            onExitMultiSelect={() => {
              setIsMultiSelectMode(false);
              deselectAll();
              $selectedId.set(null);
            }}
            listDensity={$listDensity.density()}
            onToggleDensity={toggleListDensity}
            onNewTask={openNewTaskModal}
            onOpenSettings={$ui.openSettings}
            onPauseAll={pauseAll}
            onResumeAll={resumeAll}
            onCancelAll={cancelAll}
            groupBy={$taskListView.groupBy()}
            onToggleGroupBy={toggleGroupBy}
          />

          <div class="flex flex-1 overflow-hidden relative">
            <TaskList
              tasks={$taskFilter.filteredTasks()}
              selectedTaskId={$selectedId.get()}
              groupBy={$taskListView.groupBy()}
              onTaskClick={handleTaskClick}
              onTaskContextMenu={openContextMenu}
              isMultiSelectMode={isMultiSelectMode()}
              selectedTaskIds={$selectedIds.get()}
              density={$listDensity.density()}
              searchQuery={$taskFilter.searchQuery()}
              onNewTask={openNewTaskModal}
              keyboardHandlers={{
                onTaskActivate: handleTaskActivate,
                onSelectRange: handleSelectRange,
                onSelectAll: handleSelectAll,
                onDeleteSelected: handleDeleteSelected,
              }}
            />

            <DetailPanel
              task={$selectedTask.get()}
              onClose={handleDetailClose}
              variant={isWideScreen() ? "side" : "overlay"}
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
        <Suspense fallback={<Skeleton variant="dialog" />}>
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
            api.openFolder(getParentDirectory(task.savePath)).catch(() => {
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
        <Suspense fallback={<Skeleton variant="panel" />}>
          <SnifferPanel
            visible={$ui.snifferVisible()}
            resources={snifferResources()}
            captureConfig={snifferConfig()}
            onClose={$ui.closeSniffer}
            onAddDownload={handleAddFromSniffer}
            onAddResource={handleAddSnifferResource}
            onClearResources={handleClearSnifferResources}
            onUpdateConfig={handleUpdateSnifferConfig}
          />
        </Suspense>
      </Show>

      <Show when={$ui.historyVisible()}>
        <Suspense fallback={<Skeleton variant="panel" />}>
          <HistoryPanel
            visible={$ui.historyVisible()}
            tasks={$tasks.get()}
            onClose={$ui.closeHistory}
            onOpenFolder={(folderPath) => {
              // HistoryPanel 内部已将 savePath 转为父目录,folderPath 即为要打开的文件夹路径
              if (folderPath) {
                api.openFolder(folderPath).catch(() => {
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
        <Suspense fallback={<Skeleton variant="panel" />}>
          <SettingsPanel
            visible={$ui.settingsVisible()}
            initialTab={$ui.settingsInitialTab() ?? undefined}
            onClose={$ui.closeSettings}
          />
        </Suspense>
      </Show>

      <Show when={$ui.hubVisible()}>
        <Suspense fallback={<Skeleton variant="panel" />}>
          <HfBrowserPanel visible={$ui.hubVisible()} onClose={$ui.closeHub} />
        </Suspense>
      </Show>

      <Show when={$ui.commandPaletteOpen()}>
        <Suspense fallback={<Skeleton variant="list" />}>
          <CommandPalette
            open={$ui.commandPaletteOpen()}
            onClose={$ui.closeCommandPalette}
            onViewChange={handleViewChange}
            onNewDownload={openNewTaskModal}
            onPauseAll={pauseAll}
            onResumeAll={resumeAll}
            onCancelAll={cancelAll}
            onClearCompleted={clearCompleted}
            onToggleSidebar={$ui.toggleSidebar}
            getTasks={() => $tasks.get()}
            onOpenTask={(taskId) => $selectedId.set(taskId)}
            getSelectedTask={() => $selectedTask.get()}
            onOpenTaskFolder={(taskId) => {
              const task = $tasks.get().find((t) => t.id === taskId);
              if (task?.savePath) {
                api.openFolder(getParentDirectory(task.savePath)).catch(() => {
                  addToast(tr("toast.openFolderFailed"), "error");
                });
              } else {
                addToast(tr("toast.noSavePath"), "info");
              }
            }}
            onRedownloadTask={(taskId) => {
              const task = $tasks.get().find((t) => t.id === taskId);
              if (task) handleRedownload(task);
            }}
            onCopyToClipboard={(text) => navigator.clipboard.writeText(text)}
          />
        </Suspense>
      </Show>

      <Show when={$ui.shortcutHelpOpen()}>
        <Suspense fallback={<Skeleton variant="dialog" />}>
          <ShortcutHelp
            visible={$ui.shortcutHelpOpen()}
            onClose={closeShortcutHelp}
          />
        </Suspense>
      </Show>
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
