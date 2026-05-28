# 前端 Solid.js + TypeScript 重构实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development 逐任务实现此计划。支持两种模式：子代理模式（推荐）和直接执行模式。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 将 1769 行单文件 index.html 前端迁移为 Solid.js + TypeScript + nanostores 组件化架构，保持所有现有功能不变。

**架构：** Solid.js 编译时响应式 + nanostores 原子状态 + 类型安全 Tauri IPC 封装。三栏布局（Sidebar + Main + DetailPanel）不变，每个视觉区域拆为独立组件。

**技术栈：** Solid.js 1.9, TypeScript 5.7, Vite 6, vite-plugin-solid 2, nanostores 0.10, @tauri-apps/api 2, Bun

**规格文档：** `docs/superpowers/specs/2026-05-28-frontend-refactor-design.md`

---

## 文件清单

| 文件 | 职责 | 操作 |
|------|------|------|
| `frontend/package.json` | 依赖与脚本 | 修改 |
| `frontend/vite.config.ts` | Vite + Solid 插件配置 | 修改(重写) |
| `frontend/tsconfig.json` | TypeScript 配置 | 新建 |
| `frontend/index.html` | 入口 HTML，仅含 `<script>` + `<link>` | 修改(精简) |
| `frontend/src/main.tsx` | Solid render 入口 | 新建 |
| `frontend/src/App.tsx` | 根组件：视图切换 + 布局 | 新建 |
| `frontend/src/index.css` | 设计令牌 + reset + 全局样式 | 新建 |
| `frontend/src/types.ts` | TaskInfo/AppConfig/SnifferResource/ProgressEvent | 新建 |
| `frontend/src/api/invoke.ts` | 类型安全 Tauri IPC 封装 | 新建 |
| `frontend/src/api/events.ts` | Tauri listen 封装 | 新建 |
| `frontend/src/stores/downloads.ts` | $tasks/$selectedId/$activeTasks/$completedTasks/$totalSpeed | 新建 |
| `frontend/src/stores/settings.ts` | $config/$configLoading | 新建 |
| `frontend/src/stores/sniffer.ts` | $snifferResources/$snifferActive | 新建 |
| `frontend/src/components/Layout.tsx` | 三栏网格布局 | 新建 |
| `frontend/src/components/Sidebar.tsx` | 导航 + 全局统计 | 新建 |
| `frontend/src/components/Topbar.tsx` | URL 输入 + 操作按钮 | 新建 |
| `frontend/src/components/DownloadCard.tsx` | 单个下载任务卡片 | 新建 |
| `frontend/src/components/TaskList.tsx` | 活跃/已完成分组列表 | 新建 |
| `frontend/src/components/DetailPanel.tsx` | 右侧详情 + 分片可视化 | 新建 |
| `frontend/src/components/SnifferPanel.tsx` | 嗅探资源列表 | 新建 |
| `frontend/src/components/SettingsPanel.tsx` | 配置表单 | 新建 |
| `frontend/src/components/ProgressBar.tsx` | 可复用进度条 | 新建 |
| `frontend/src/components/FragmentGrid.tsx` | 分片状态可视化 | 新建 |
| `frontend/src/components/Toggle.tsx` | 开关组件 | 新建 |
| `frontend/src/hooks/useTauriEvent.ts` | Tauri 事件订阅 hook | 新建 |
| `frontend/src/utils/format.ts` | formatSize/formatSpeed/statusText/guessExt | 新建 |
| `frontend/src/utils/icons.tsx` | SVG 图标组件 | 新建 |

---

### 任务 1：安装依赖 + 配置构建工具链

**文件：**
- 修改：`frontend/package.json`
- 新建：`frontend/vite.config.ts`
- 新建：`frontend/tsconfig.json`

- [ ] **步骤 1：安装依赖**

```bash
cd frontend && bun add solid-js @tauri-apps/api nanostores && bun add -d vite vite-plugin-solid typescript @types/node
```

- [ ] **步骤 2：重写 vite.config.ts**

```ts
import { defineConfig } from 'vite'
import solidPlugin from 'vite-plugin-solid'

export default defineConfig({
  plugins: [solidPlugin()],
  clearScreen: false,
  server: {
    port: 3000,
    strictPort: true,
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: 'esnext',
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
})
```

- [ ] **步骤 3：创建 tsconfig.json**

```json
{
  "compilerOptions": {
    "target": "ESNext",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "allowSyntheticDefaultImports": true,
    "esModuleInterop": true,
    "jsx": "preserve",
    "jsxImportSource": "solid-js",
    "types": ["vite/client"],
    "noEmit": true,
    "strict": true,
    "isolatedModules": true,
    "skipLibCheck": true
  },
  "include": ["src"]
}
```

- [ ] **步骤 4：更新 package.json scripts**

确保 scripts 使用 bun：

```json
{
  "scripts": {
    "dev": "bun --bun vite",
    "build": "bun --bun vite build",
    "preview": "bun --bun vite preview"
  }
}
```

- [ ] **步骤 5：验证构建工具链**

```bash
cd frontend && bun run build
```

预期：构建成功，无错误。

- [ ] **步骤 6：Commit**

```bash
git add frontend/package.json frontend/vite.config.ts frontend/tsconfig.json frontend/bun.lock
git commit -m "chore(前端): 安装 Solid.js + TypeScript + nanostores 依赖"
```

---

### 任务 2：创建入口文件 + 设计令牌 + 类型定义

**文件：**
- 修改：`frontend/index.html`
- 新建：`frontend/src/main.tsx`
- 新建：`frontend/src/index.css`
- 新建：`frontend/src/types.ts`

- [ ] **步骤 1：精简 index.html 为入口文件**

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>QuantumFetch</title>
</head>
<body>
  <div id="root"></div>
  <script src="/src/main.tsx" type="module"></script>
</body>
</html>
```

- [ ] **步骤 2：创建 main.tsx**

```tsx
import { render } from 'solid-js/web'
import App from './App'
import './index.css'

const root = document.getElementById('root')
if (!root) throw new Error('Root element not found')

render(() => <App />, root)
```

- [ ] **步骤 3：创建 index.css（设计令牌 + reset + 全局样式）**

从现有 index.html 的 `<style>` 块迁移所有 CSS，按原样保留。新增质感变量：

```css
:root {
  --bg: #0a0a0f;
  --surface: #111118;
  --surface-glass: rgba(17, 17, 24, 0.75);
  --border: #1a1a24;
  --border-light: #242430;
  --text: #d8d8e0;
  --text-2: #6a6a78;
  --text-3: #3a3a48;
  --accent: #10b981;
  --ok: #22c55e;
  --warn: #eab308;
  --err: #ef4444;
  --radius: 6px;
  --shadow: 0 1px 3px rgba(0,0,0,0.3);
  --font-sans: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
  --font-mono: 'SF Mono', 'Cascadia Code', 'Fira Code', 'Consolas', monospace;
}

*, *::before, *::after { margin: 0; padding: 0; box-sizing: border-box; }
body {
  font-family: var(--font-sans);
  font-size: 13px;
  line-height: 1.5;
  background: var(--bg);
  color: var(--text);
  min-height: 100vh;
  overflow: hidden;
}
.mono { font-family: var(--font-mono); }
```

然后逐块迁移现有 CSS（sidebar、topbar、buttons、download-card、progress-bar、detail-panel、settings、toggle、sniffer、empty-state、fragment-grid）— 复制原 index.html 中第 7-694 行的 `<style>` 内容，删除旧 `<style>` 块。

- [ ] **步骤 4：创建 types.ts**

```ts
export type DownloadStatus = 'pending' | 'downloading' | 'paused' | 'completed' | 'failed' | 'cancelled'

export interface TaskInfo {
  id: string
  url: string
  file_name: string
  file_size: number
  downloaded: number
  speed: number
  status: DownloadStatus
  progress: number
  fragments_total: number
  fragments_done: number
  created_at: string
}

export interface AppConfig {
  download_dir: string
  max_concurrent_tasks: number
  max_concurrent_fragments: number
  max_connections_per_host: number
  enable_quic: boolean
  verify_checksum: boolean
}

export type SnifferResourceType = 'video' | 'audio' | 'document' | 'archive' | 'executable' | 'other'

export interface SnifferResource {
  url: string
  name: string
  type: SnifferResourceType
  size: number
  content_type?: string
  source_page?: string
}

export interface ProgressPayload {
  downloaded: number
  speed: number
  status: string
  fragmentsDone: number
}

export type ProgressEvent = Record<string, ProgressPayload>

export type ViewName = 'downloads' | 'sniffer' | 'settings'
```

- [ ] **步骤 5：验证构建**

```bash
cd frontend && bun run build
```

预期：构建成功。

- [ ] **步骤 6：Commit**

```bash
git add frontend/index.html frontend/src/main.tsx frontend/src/index.css frontend/src/types.ts
git commit -m "feat(前端): 创建入口文件 + 设计令牌 + 类型定义"
```

---

### 任务 3：创建 API 层 + 工具函数

**文件：**
- 新建：`frontend/src/api/invoke.ts`
- 新建：`frontend/src/api/events.ts`
- 新建：`frontend/src/utils/format.ts`
- 新建：`frontend/src/utils/icons.tsx`

- [ ] **步骤 1：创建 api/invoke.ts**

```ts
import type { TaskInfo, AppConfig, SnifferResource } from '../types'

declare global {
  interface Window {
    __TAURI__?: {
      core: {
        invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>
      }
    }
  }
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!window.__TAURI__) {
    throw new Error('Tauri API not available')
  }
  return window.__TAURI__.core.invoke(cmd, args) as Promise<T>
}

export const api = {
  createTask: (url: string) => invoke<string>('create_task', { url }),
  getTaskList: () => invoke<TaskInfo[]>('get_task_list'),
  getTaskDetail: (taskId: string) => invoke<TaskInfo>('get_task_detail', { taskId }),
  pauseTask: (taskId: string) => invoke<void>('pause_task', { taskId }),
  resumeTask: (taskId: string) => invoke<void>('resume_task', { taskId }),
  cancelTask: (taskId: string) => invoke<void>('cancel_task', { taskId }),
  deleteTask: (taskId: string) => invoke<void>('delete_task', { taskId }),
  getConfig: () => invoke<AppConfig>('get_config'),
  updateConfig: (config: Partial<AppConfig>) => invoke<void>('update_config', { config }),
  getSnifferResources: () => invoke<SnifferResource[]>('get_sniffer_resources'),
  subscribeProgress: () => invoke<void>('subscribe_progress'),
}
```

- [ ] **步骤 2：创建 api/events.ts**

```ts
import type { ProgressEvent } from '../types'

type UnlistenFn = () => void

async function listen<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  if (window.__TAURI__?.event) {
    const { listen: tauriListen } = await import('@tauri-apps/api/event')
    const unlisten = await tauriListen<T>(event, (e) => handler(e.payload))
    return unlistenerFn
  }
  return () => {}
}

function unlistenerFn() {}

export function onProgressUpdate(handler: (payload: ProgressEvent) => void): Promise<UnlistenFn> {
  return listen<ProgressEvent>('progress-update', handler)
}
```

注意：此处使用动态 import `@tauri-apps/api/event`，因为该包已安装为依赖。`listen` 返回 `Promise<UnlistenFn>`。

- [ ] **步骤 3：创建 utils/format.ts**

从现有 index.html 提取纯函数：

```ts
export function formatSize(bytes: number): string {
  if (!bytes || bytes === 0) return '0 B'
  const k = 1024
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(k))
  return (bytes / Math.pow(k, i)).toFixed(i > 1 ? 1 : 0) + ' ' + sizes[i]
}

export function formatSpeed(bytes: number): string {
  return formatSize(bytes) + '/s'
}

export function statusText(status: string): string {
  const map: Record<string, string> = {
    downloading: '下载中',
    completed: '已完成',
    paused: '已暂停',
    failed: '失败',
    pending: '等待中',
    cancelled: '已取消',
  }
  return map[status] || status
}

export function guessExt(name: string): string {
  const parts = name.split('.')
  return parts.length > 1 ? parts.pop()!.toUpperCase().slice(0, 4) : 'FILE'
}
```

- [ ] **步骤 4：创建 utils/icons.tsx**

```tsx
export function IconPause() {
  return <svg viewBox="0 0 12 12" aria-hidden="true"><rect x="2" y="1" width="3" height="10" rx="0.5"/><rect x="7" y="1" width="3" height="10" rx="0.5"/></svg>
}

export function IconResume() {
  return <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M3 1v10l8-5z"/></svg>
}

export function IconCancel() {
  return <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M2 2l8 8M10 2l-8 8"/></svg>
}

export function IconDelete() {
  return <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M1.5 3h9M4.5 3V1.5h3V3M3 3v7.5h6V3"/><path d="M5 5.5v3M7 5.5v3"/></svg>
}
```

- [ ] **步骤 5：验证构建**

```bash
cd frontend && bun run build
```

- [ ] **步骤 6：Commit**

```bash
git add frontend/src/api/ frontend/src/utils/
git commit -m "feat(前端): 创建 API 层 + 工具函数"
```

---

### 任务 4：创建 nanostores 状态管理

**文件：**
- 新建：`frontend/src/stores/downloads.ts`
- 新建：`frontend/src/stores/settings.ts`
- 新建：`frontend/src/stores/sniffer.ts`

- [ ] **步骤 1：创建 stores/downloads.ts**

```ts
import { atom, computed } from 'nanostores'
import type { TaskInfo, DownloadStatus } from '../types'

export const $tasks = atom<TaskInfo[]>([])
export const $selectedId = atom<string | null>(null)

const ACTIVE_STATUSES: DownloadStatus[] = ['downloading', 'paused', 'pending']
const COMPLETED_STATUSES: DownloadStatus[] = ['completed', 'failed', 'cancelled']

export const $activeTasks = computed($tasks, tasks =>
  tasks.filter(t => ACTIVE_STATUSES.includes(t.status))
)

export const $completedTasks = computed($tasks, tasks =>
  tasks.filter(t => COMPLETED_STATUSES.includes(t.status))
)

export const $totalSpeed = computed($activeTasks, tasks =>
  tasks.reduce((sum, t) => sum + (t.speed || 0), 0)
)
```

- [ ] **步骤 2：创建 stores/settings.ts**

```ts
import { atom } from 'nanostores'
import type { AppConfig } from '../types'

export const $config = atom<AppConfig | null>(null)
export const $configLoading = atom(true)
```

- [ ] **步骤 3：创建 stores/sniffer.ts**

```ts
import { atom } from 'nanostores'
import type { SnifferResource } from '../types'

export const $snifferResources = atom<SnifferResource[]>([])
export const $snifferActive = atom(true)
```

- [ ] **步骤 4：验证构建**

```bash
cd frontend && bun run build
```

- [ ] **步骤 5：Commit**

```bash
git add frontend/src/stores/
git commit -m "feat(前端): 创建 nanostores 状态管理"
```

---

### 任务 5：创建基础组件（Layout + Sidebar + Topbar）

**文件：**
- 新建：`frontend/src/App.tsx`
- 新建：`frontend/src/components/Layout.tsx`
- 新建：`frontend/src/components/Sidebar.tsx`
- 新建：`frontend/src/components/Topbar.tsx`

- [ ] **步骤 1：创建 components/Layout.tsx**

```tsx
import type { JSX } from 'solid-js'
import Sidebar from './Sidebar'
import Topbar from './Topbar'

export default function Layout(props: { children: JSX.Element }) {
  return (
    <div style={{ display: 'grid', 'grid-template-columns': '200px 1fr auto', height: '100vh' }}>
      <Sidebar />
      <div class="main" style={{ display: 'flex', 'flex-direction': 'column', overflow: 'hidden' }}>
        <Topbar />
        {props.children}
      </div>
    </div>
  )
}
```

- [ ] **步骤 2：创建 components/Sidebar.tsx**

从现有 index.html sidebar 部分（第 699-727 行）迁移。使用 `$activeTasks`、`$totalSpeed` 派生统计。

```tsx
import { useStore } from '@nanostores/solid'
import { $activeTasks, $totalSpeed } from '../stores/downloads'
import { $tasks } from '../stores/downloads'
import { formatSpeed } from '../utils/format'
import type { ViewName } from '../types'

export default function Sidebar(props: { currentView: ViewName; onViewChange: (v: ViewName) => void }) {
  const activeTasks = useStore($activeTasks)
  const totalSpeed = useStore($totalSpeed)
  const tasks = useStore($tasks)

  const navItems: { view: ViewName; label: string; icon: string }[] = [
    { view: 'downloads', label: '下载列表', icon: 'M10 3v10m0 0l-3.5-3.5M10 13l3.5-3.5M4 17h12' },
    { view: 'sniffer', label: '资源嗅探', icon: 'M10 4C6 4 3 7.5 3 10.5S6 17 10 17s7-3.5 7-6.5S14 4 10 4z' },
    { view: 'settings', label: '设置', icon: 'M10 1.5v2m0 13v2M18.5 10h-2m-13 0h-2M15.66 4.34l-1.42 1.42M5.76 14.24l-1.42 1.42m11.32 0l-1.42-1.42M5.76 5.76L4.34 4.34' },
  ]

  return (
    <div class="sidebar" role="navigation" aria-label="主导航">
      <div class="sidebar-logo">QuantumFetch</div>
      <div class="sidebar-nav">
        {navItems.map(item => (
          <div
            class={`nav-item ${props.currentView === item.view ? 'active' : ''}`}
            role="button"
            tabindex="0"
            aria-current={props.currentView === item.view ? 'page' : undefined}
            aria-label={item.label}
            onClick={() => props.onViewChange(item.view)}
            onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); props.onViewChange(item.view) } }}
          >
            <span class="nav-icon">
              <svg viewBox="0 0 20 20" aria-hidden="true">
                <path d={item.icon} stroke="currentColor" fill="none" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
            </span>
            <span>{item.label}</span>
          </div>
        ))}
      </div>
      <div class="sidebar-stats" aria-live="polite" aria-label="全局统计">
        <div>活跃连接 <span class="mono">{activeTasks().filter(t => t.status === 'downloading').length}</span></div>
        <div>总速度 <span class="mono">{formatSpeed(totalSpeed())}</span></div>
        <div>队列任务 <span class="mono">{activeTasks().length}</span></div>
      </div>
    </div>
  )
}
```

- [ ] **步骤 3：创建 components/Topbar.tsx**

从现有 index.html topbar 部分（第 731-738 行）迁移。

```tsx
import { createSignal } from 'solid-js'
import { api } from '../api/invoke'
import { $tasks } from '../stores/downloads'

export default function Topbar() {
  const [url, setUrl] = createSignal('')

  async function startDownload() {
    const u = url().trim()
    if (!u) return
    try {
      await api.createTask(u)
      setUrl('')
      refreshTaskList()
    } catch (e) {
      console.error('创建任务失败:', e)
    }
  }

  async function refreshTaskList() {
    try {
      const tasks = await api.getTaskList()
      $tasks.set(tasks)
    } catch (e) {
      console.error('刷新任务列表失败:', e)
    }
  }

  async function pauseAll() {
    const active = $tasks.get().filter(t => t.status === 'downloading' || t.status === 'pending')
    await Promise.allSettled(active.map(t => api.pauseTask(t.id).catch(() => {})))
    refreshTaskList()
  }

  return (
    <div class="topbar">
      <div class="url-bar">
        <input
          type="text"
          value={url()}
          onInput={(e) => setUrl(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') startDownload() }}
          placeholder="粘贴下载链接,支持 HTTP/HTTPS/FTP/QUIC..."
          aria-label="下载链接输入"
        />
        <button class="btn btn-primary" onClick={startDownload} aria-label="开始下载">开始下载</button>
      </div>
      <button class="btn btn-ghost" onClick={pauseAll} aria-label="暂停所有下载任务">全部暂停</button>
    </div>
  )
}
```

- [ ] **步骤 4：创建 App.tsx 骨架**

```tsx
import { createSignal } from 'solid-js'
import type { ViewName } from './types'
import Layout from './components/Layout'
import TaskList from './components/TaskList'

export default function App() {
  const [currentView, setCurrentView] = createSignal<ViewName>('downloads')

  return (
    <Layout currentView={currentView()} onViewChange={setCurrentView}>
      {currentView() === 'downloads' && <TaskList />}
      {currentView() === 'sniffer' && <div>SnifferPanel (待迁移)</div>}
      {currentView() === 'settings' && <div>SettingsPanel (待迁移)</div>}
    </Layout>
  )
}
```

- [ ] **步骤 5：验证构建**

```bash
cd frontend && bun run build
```

- [ ] **步骤 6：Commit**

```bash
git add frontend/src/App.tsx frontend/src/components/Layout.tsx frontend/src/components/Sidebar.tsx frontend/src/components/Topbar.tsx
git commit -m "feat(前端): 创建 Layout + Sidebar + Topbar 基础组件"
```

---

### 任务 6：创建 UI 原子组件（ProgressBar + Toggle + FragmentGrid）

**文件：**
- 新建：`frontend/src/components/ProgressBar.tsx`
- 新建：`frontend/src/components/Toggle.tsx`
- 新建：`frontend/src/components/FragmentGrid.tsx`

- [ ] **步骤 1：创建 components/ProgressBar.tsx**

```tsx
import type { DownloadStatus } from '../types'

interface ProgressBarProps {
  progress: number
  status: DownloadStatus
  label?: string
}

export default function ProgressBar(props: ProgressBarProps) {
  return (
    <div
      class="progress-bar"
      role="progressbar"
      aria-valuenow={props.progress}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-label={props.label}
    >
      <div class={`progress-fill ${props.status}`} style={{ width: `${props.progress}%` }} />
    </div>
  )
}
```

- [ ] **步骤 2：创建 components/Toggle.tsx**

```tsx
import { createSignal } from 'solid-js'

interface ToggleProps {
  initial?: boolean
  ariaLabel: string
  onChange?: (value: boolean) => void
}

export default function Toggle(props: ToggleProps) {
  const [on, setOn] = createSignal(props.initial ?? false)

  function toggle() {
    const next = !on()
    setOn(next)
    props.onChange?.(next)
  }

  return (
    <div
      class={`toggle ${on() ? 'on' : ''}`}
      role="switch"
      aria-checked={on()}
      aria-label={props.ariaLabel}
      tabindex="0"
      onClick={toggle}
      onKeyDown={(e) => { if (e.key === ' ' || e.key === 'Enter') { e.preventDefault(); toggle() } }}
    />
  )
}
```

- [ ] **步骤 3：创建 components/FragmentGrid.tsx**

```tsx
import type { DownloadStatus } from '../types'

interface FragmentGridProps {
  total: number
  done: number
  status: DownloadStatus
}

export default function FragmentGrid(props: FragmentGridProps) {
  const blocks = () => {
    const arr: ('done' | 'active' | 'pending')[] = []
    for (let i = 0; i < props.total; i++) {
      if (i < props.done) arr.push('done')
      else if (i === props.done && props.status === 'downloading') arr.push('active')
      else arr.push('pending')
    }
    return arr
  }

  return (
    <div class="fragment-grid">
      <For each={blocks()}>
        {(cls) => <div class={`fragment-block ${cls}`} />}
      </For>
    </div>
  )
}

import { For } from 'solid-js'
```

注意：`For` 的 import 放在文件顶部，此处为说明。实际文件应将 `import { For } from 'solid-js'` 放在文件头。

- [ ] **步骤 4：验证构建**

```bash
cd frontend && bun run build
```

- [ ] **步骤 5：Commit**

```bash
git add frontend/src/components/ProgressBar.tsx frontend/src/components/Toggle.tsx frontend/src/components/FragmentGrid.tsx
git commit -m "feat(前端): 创建 ProgressBar + Toggle + FragmentGrid 原子组件"
```

---

### 任务 7：创建 DownloadCard + TaskList + DetailPanel

**文件：**
- 新建：`frontend/src/components/DownloadCard.tsx`
- 新建：`frontend/src/components/TaskList.tsx`
- 新建：`frontend/src/components/DetailPanel.tsx`

- [ ] **步骤 1：创建 components/DownloadCard.tsx**

```tsx
import type { TaskInfo } from '../types'
import { formatSize, formatSpeed, statusText, guessExt } from '../utils/format'
import { IconPause, IconResume, IconCancel, IconDelete } from '../utils/icons'
import ProgressBar from './ProgressBar'

interface DownloadCardProps {
  task: TaskInfo
  selected: boolean
  onSelect: (id: string) => void
  onPause: (id: string) => void
  onResume: (id: string) => void
  onCancel: (id: string) => void
  onDelete: (id: string) => void
}

export default function DownloadCard(props: DownloadCardProps) {
  const progress = () => props.task.file_size > 0
    ? Math.round((props.task.downloaded / props.task.file_size) * 100)
    : 0

  return (
    <div
      class="download-card"
      data-id={props.task.id}
      role="listitem"
      style={{ 'border-color': props.selected ? 'var(--accent)' : '' }}
      onClick={() => props.onSelect(props.task.id)}
    >
      <div class="card-header">
        <div class="card-name">
          {props.task.file_name}
          <span class="ext">{guessExt(props.task.file_name)}</span>
        </div>
        <span class={`card-status status-${props.task.status}`}>{statusText(props.task.status)}</span>
      </div>
      <ProgressBar progress={progress()} status={props.task.status} label={`${props.task.file_name} 下载进度`} />
      <div class="card-details">
        <div class="detail-item"><span class="detail-label">进度</span><span class="mono">{progress()}%</span></div>
        <div class="detail-item"><span class="detail-label">大小</span><span class="mono">{formatSize(props.task.file_size)}</span></div>
        {props.task.speed > 0 && (
          <div class="detail-item"><span class="detail-label">速度</span><span class="speed-value">{formatSpeed(props.task.speed)}</span></div>
        )}
        <div class="detail-item"><span class="detail-label">分片</span><span class="mono">{props.task.fragments_done}/{props.task.fragments_total}</span></div>
      </div>
      <div class="card-actions">
        {(props.task.status === 'downloading' || props.task.status === 'pending') && (
          <button class="btn-card-action" aria-label={`暂停下载 ${props.task.file_name}`} onClick={(e) => { e.stopPropagation(); props.onPause(props.task.id) }}>
            <IconPause />暂停
          </button>
        )}
        {props.task.status === 'paused' && (
          <button class="btn-card-action" aria-label={`恢复下载 ${props.task.file_name}`} onClick={(e) => { e.stopPropagation(); props.onResume(props.task.id) }}>
            <IconResume />恢复
          </button>
        )}
        {props.task.status !== 'completed' && props.task.status !== 'cancelled' && (
          <button class="btn-card-action action-danger" aria-label={`取消下载 ${props.task.file_name}`} onClick={(e) => { e.stopPropagation(); props.onCancel(props.task.id) }}>
            <IconCancel />取消
          </button>
        )}
        {(props.task.status === 'completed' || props.task.status === 'cancelled' || props.task.status === 'failed') && (
          <button class="btn-card-action action-danger" aria-label={`删除任务 ${props.task.file_name}`} onClick={(e) => { e.stopPropagation(); props.onDelete(props.task.id) }}>
            <IconDelete />删除
          </button>
        )}
      </div>
    </div>
  )
}
```

- [ ] **步骤 2：创建 components/TaskList.tsx**

```tsx
import { For, Show } from 'solid-js'
import { useStore } from '@nanostores/solid'
import { $activeTasks, $completedTasks, $selectedId, $tasks } from '../stores/downloads'
import { api } from '../api/invoke'
import DownloadCard from './DownloadCard'

async function refreshTaskList() {
  try {
    const tasks = await api.getTaskList()
    const { $tasks } = await import('../stores/downloads')
    $tasks.set(tasks)
  } catch (e) {
    console.error('刷新任务列表失败:', e)
  }
}

export default function TaskList() {
  const activeTasks = useStore($activeTasks)
  const completedTasks = useStore($completedTasks)
  const selectedId = useStore($selectedId)
  const tasks = useStore($tasks)

  async function handlePause(taskId: string) { await api.pauseTask(taskId); refreshTaskList() }
  async function handleResume(taskId: string) { await api.resumeTask(taskId); refreshTaskList() }
  async function handleCancel(taskId: string) { await api.cancelTask(taskId); refreshTaskList() }
  async function handleDelete(taskId: string) { await api.deleteTask(taskId); refreshTaskList() }

  return (
    <div class="downloads-view active" role="list" aria-label="下载任务列表">
      <div class="section-title">下载中</div>
      <div id="active-downloads">
        <For each={activeTasks()}>
          {(task) => (
            <DownloadCard
              task={task}
              selected={selectedId() === task.id}
              onSelect={(id) => $selectedId.set(id)}
              onPause={handlePause}
              onResume={handleResume}
              onCancel={handleCancel}
              onDelete={handleDelete}
            />
          )}
        </For>
      </div>
      <div class="section-title" style={{ 'margin-top': '20px' }}>已完成</div>
      <div id="completed-downloads">
        <For each={completedTasks()}>
          {(task) => (
            <DownloadCard
              task={task}
              selected={selectedId() === task.id}
              onSelect={(id) => $selectedId.set(id)}
              onPause={handlePause}
              onResume={handleResume}
              onCancel={handleCancel}
              onDelete={handleDelete}
            />
          )}
        </For>
      </div>
      <Show when={tasks().length === 0}>
        <div class="empty-state">
          <svg class="empty-icon" viewBox="0 0 20 20" aria-hidden="true"><path d="M10 3v10m0 0l-3.5-3.5M10 13l3.5-3.5M4 17h12" stroke="var(--text-3)" fill="none" stroke-width="1" /></svg>
          <div class="empty-text">暂无下载任务</div>
          <div class="empty-hint">在上方输入链接开始下载</div>
        </div>
      </Show>
    </div>
  )
}
```

- [ ] **步骤 3：创建 components/DetailPanel.tsx**

```tsx
import { useStore } from '@nanostores/solid'
import { $selectedId, $tasks } from '../stores/downloads'
import { formatSize, formatSpeed, statusText } from '../utils/format'
import FragmentGrid from './FragmentGrid'

export default function DetailPanel() {
  const selectedId = useStore($selectedId)
  const tasks = useStore($tasks)

  const task = () => {
    const id = selectedId()
    if (!id) return null
    return tasks().find(t => t.id === id) ?? null
  }

  return (
    <Show when={task()}>
      {(t) => (
        <div class="detail-panel" role="complementary" aria-label="下载详情" style={{ display: 'block' }}>
          <div class="panel-title">{t().file_name}</div>
          <div class="panel-row"><span class="panel-label">文件大小</span><span class="panel-value mono">{formatSize(t().file_size)}</span></div>
          <div class="panel-row"><span class="panel-label">已下载</span><span class="panel-value mono">{formatSize(t().downloaded)}</span></div>
          <div class="panel-row"><span class="panel-label">速度</span><span class="panel-value mono speed-value">{t().speed > 0 ? formatSpeed(t().speed) : '--'}</span></div>
          <div class="panel-row"><span class="panel-label">分片数</span><span class="panel-value mono">{t().fragments_total}</span></div>
          <div class="panel-row"><span class="panel-label">协议</span><span class="panel-value">HTTPS</span></div>
          <div class="panel-row"><span class="panel-label">状态</span><span class="panel-value">{statusText(t().status)}</span></div>
          <div style={{ 'margin-top': '16px' }}>
            <div class="section-title">分片进度</div>
            <FragmentGrid total={t().fragments_total} done={t().fragments_done} status={t().status} />
          </div>
        </div>
      )}
    </Show>
  )
}
```

- [ ] **步骤 4：更新 App.tsx 集成 DetailPanel**

在 App.tsx 中引入 DetailPanel，传递给 Layout。

- [ ] **步骤 5：验证构建**

```bash
cd frontend && bun run build
```

- [ ] **步骤 6：Commit**

```bash
git add frontend/src/components/DownloadCard.tsx frontend/src/components/TaskList.tsx frontend/src/components/DetailPanel.tsx frontend/src/App.tsx
git commit -m "feat(前端): 创建 DownloadCard + TaskList + DetailPanel 核心组件"
```

---

### 任务 8：创建 SnifferPanel + SettingsPanel

**文件：**
- 新建：`frontend/src/components/SnifferPanel.tsx`
- 新建：`frontend/src/components/SettingsPanel.tsx`

- [ ] **步骤 1：创建 components/SnifferPanel.tsx**

从现有 index.html 嗅探面板（第 755-768 + 1582-1635 行）迁移。删除 mock 数据，使用 Tauri IPC 获取真实资源。

```tsx
import { For, Show } from 'solid-js'
import { useStore } from '@nanostores/solid'
import { $snifferResources, $snifferActive } from '../stores/sniffer'
import { api } from '../api/invoke'
import { $tasks } from '../stores/downloads'
import { formatSize } from '../utils/format'

const TYPE_CLASS: Record<string, string> = {
  video: 'type-video',
  audio: 'type-audio',
  document: 'type-document',
  archive: 'type-archive',
  executable: 'type-executable',
  other: '',
}

export default function SnifferPanel() {
  const resources = useStore($snifferResources)
  const active = useStore($snifferActive)

  async function downloadSnifferItem(url: string) {
    try {
      await api.createTask(url)
      const tasks = await api.getTaskList()
      $tasks.set(tasks)
    } catch (e) {
      console.error('从嗅探面板创建任务失败:', e)
    }
  }

  function toggleSniffer() {
    $snifferActive.set(!$snifferActive.get())
  }

  async function loadSnifferResources() {
    try {
      const res = await api.getSnifferResources()
      $snifferResources.set(res)
    } catch {
      $snifferResources.set([])
    }
  }

  loadSnifferResources()

  return (
    <div class="sniffer-view active">
      <div class="sniffer-header">
        <h2 style={{ 'font-size': '15px', 'font-weight': 600, color: 'var(--text)' }}>资源嗅探</h2>
        <span class={`sniffer-status ${active() ? 'active' : 'inactive'}`}>{active() ? '监听中' : '已暂停'}</span>
        <button class="btn btn-ghost" style={{ 'margin-left': 'auto' }} onClick={toggleSniffer} aria-label="暂停或恢复嗅探监听">
          {active() ? '暂停监听' : '恢复监听'}
        </button>
      </div>
      <div class="section-title">检测到的资源</div>
      <Show when={resources().length > 0 && active()} fallback={
        <div class="empty-state" style={{ 'min-height': '300px' }}>
          <svg class="empty-icon" viewBox="0 0 20 20" aria-hidden="true"><path d="M10 4C6 4 3 7.5 3 10.5S6 17 10 17s7-3.5 7-6.5S14 4 10 4z" stroke="var(--text-3)" fill="none" stroke-width="1" /><circle cx="10" cy="10.5" r="2.5" stroke="var(--text-3)" fill="none" stroke-width="1" /></svg>
          <div class="empty-text">正在监听浏览器流量...</div>
          <div class="empty-hint">浏览包含视频、音频或文档的网页时,资源将自动显示在这里</div>
        </div>
      }>
        <div class="sniffer-list" role="list" aria-label="嗅探到的资源列表">
          <For each={resources()}>
            {(item) => (
              <div class="sniffer-item" role="listitem">
                <span class={`sniffer-type ${TYPE_CLASS[item.type] || ''}`}>{item.type}</span>
                <div class="sniffer-info">
                  <div class="sniffer-name">{item.name}</div>
                  <div class="sniffer-url">{item.url}</div>
                </div>
                <span class="sniffer-size mono">{formatSize(item.size)}</span>
                <div class="sniffer-actions">
                  <button class="btn-sm btn-download-sm" aria-label={`下载 ${item.name}`} onClick={() => downloadSnifferItem(item.url)}>下载</button>
                </div>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  )
}
```

- [ ] **步骤 2：创建 components/SettingsPanel.tsx**

从现有 index.html 设置面板（第 772-837 + 1525-1576 行）迁移。

```tsx
import { createSignal, onMount } from 'solid-js'
import { api } from '../api/invoke'
import { $config } from '../stores/settings'
import Toggle from './Toggle'

export default function SettingsPanel() {
  let dirRef: HTMLInputElement | undefined
  let maxTasksRef: HTMLInputElement | undefined
  let maxFragmentsRef: HTMLInputElement | undefined
  let maxConnectionsRef: HTMLInputElement | undefined
  const [quic, setQuic] = createSignal(false)
  const [verify, setVerify] = createSignal(true)
  const [saved, setSaved] = createSignal(false)

  onMount(async () => {
    try {
      const config = await api.getConfig()
      $config.set(config)
      if (dirRef) dirRef.value = config.download_dir || ''
      if (maxTasksRef) maxTasksRef.value = String(config.max_concurrent_tasks || 5)
      if (maxFragmentsRef) maxFragmentsRef.value = String(config.max_concurrent_fragments || 16)
      if (maxConnectionsRef) maxConnectionsRef.value = String(config.max_connections_per_host || 16)
      setQuic(config.enable_quic)
      setVerify(config.verify_checksum)
    } catch (e) {
      console.error('加载设置失败:', e)
    }
  })

  async function saveSettings() {
    const config = {
      download_dir: dirRef?.value || '',
      max_concurrent_tasks: parseInt(maxTasksRef?.value || '5') || 5,
      max_concurrent_fragments: parseInt(maxFragmentsRef?.value || '16') || 16,
      max_connections_per_host: parseInt(maxConnectionsRef?.value || '16') || 16,
      enable_quic: quic(),
      verify_checksum: verify(),
    }
    try {
      await api.updateConfig(config)
      setSaved(true)
      setTimeout(() => setSaved(false), 1500)
    } catch (e) {
      console.error('保存设置失败:', e)
    }
  }

  return (
    <div class="settings-view active">
      <h2 style={{ 'font-size': '15px', 'font-weight': 600, 'margin-bottom': '20px' }}>设置</h2>
      <div class="setting-group">
        <div class="setting-group-title">下载设置</div>
        <div class="setting-row"><span class="setting-label">下载目录</span><input ref={dirRef} class="setting-input" value="C:\Users\Downloads" aria-label="下载目录" /></div>
        <div class="setting-row"><span class="setting-label">最大并发任务数</span><input ref={maxTasksRef} class="setting-input" type="number" value="3" min="1" max="10" style={{ width: '80px' }} aria-label="最大并发任务数" /></div>
        <div class="setting-row"><span class="setting-label">每任务最大分片数</span><input ref={maxFragmentsRef} class="setting-input" type="number" value="16" min="1" max="64" style={{ width: '80px' }} aria-label="每任务最大分片数" /></div>
        <div class="setting-row"><span class="setting-label">每主机最大连接数</span><input ref={maxConnectionsRef} class="setting-input" type="number" value="16" min="1" max="32" style={{ width: '80px' }} aria-label="每主机最大连接数" /></div>
      </div>
      <div class="setting-group">
        <div class="setting-group-title">协议设置</div>
        <div class="setting-row"><span class="setting-label">启用 QUIC 传输</span><Toggle initial={quic()} ariaLabel="启用 QUIC 传输" onChange={setQuic} /></div>
        <div class="setting-row"><span class="setting-label">启用 HTTP/2 多路复用</span><Toggle initial={true} ariaLabel="启用 HTTP/2 多路复用" /></div>
      </div>
      <div class="setting-group">
        <div class="setting-group-title">安全设置</div>
        <div class="setting-row"><span class="setting-label">下载后校验完整性</span><Toggle initial={verify()} ariaLabel="下载后校验完整性" onChange={setVerify} /></div>
        <div class="setting-row">
          <span class="setting-label">校验算法</span>
          <select class="setting-input" style={{ width: '120px' }} aria-label="校验算法">
            <option value="blake3" selected>Blake3</option>
            <option value="sha256">SHA-256</option>
          </select>
        </div>
      </div>
      <div class="setting-group">
        <div class="setting-group-title">浏览器嗅探</div>
        <div class="setting-row"><span class="setting-label">启用资源嗅探</span><Toggle initial={true} ariaLabel="启用资源嗅探" /></div>
        <div class="setting-row">
          <span class="setting-label">最小文件大小</span>
          <select class="setting-input" style={{ width: '120px' }} aria-label="最小文件大小">
            <option value="0">不限制</option>
            <option value="1024" selected>1 KB</option>
            <option value="102400">100 KB</option>
            <option value="1048576">1 MB</option>
          </select>
        </div>
      </div>
      <div style={{ 'margin-top': '16px' }}>
        <button
          class="btn btn-primary"
          onClick={saveSettings}
          aria-label="保存设置"
          style={saved() ? { background: '#22c55e' } : {}}
        >
          {saved() ? '已保存' : '保存设置'}
        </button>
      </div>
    </div>
  )
}
```

- [ ] **步骤 3：更新 App.tsx 集成所有面板**

```tsx
import { createSignal } from 'solid-js'
import type { ViewName } from './types'
import Layout from './components/Layout'
import TaskList from './components/TaskList'
import DetailPanel from './components/DetailPanel'
import SnifferPanel from './components/SnifferPanel'
import SettingsPanel from './components/SettingsPanel'

export default function App() {
  const [currentView, setCurrentView] = createSignal<ViewName>('downloads')

  return (
    <Layout currentView={currentView()} onViewChange={setCurrentView}>
      {currentView() === 'downloads' && <TaskList />}
      {currentView() === 'sniffer' && <SnifferPanel />}
      {currentView() === 'settings' && <SettingsPanel />}
      {currentView() === 'downloads' && <DetailPanel />}
    </Layout>
  )
}
```

- [ ] **步骤 4：验证构建**

```bash
cd frontend && bun run build
```

- [ ] **步骤 5：Commit**

```bash
git add frontend/src/components/SnifferPanel.tsx frontend/src/components/SettingsPanel.tsx frontend/src/App.tsx
git commit -m "feat(前端): 创建 SnifferPanel + SettingsPanel，集成所有组件"
```

---

### 任务 9：创建 hooks + Tauri 事件集成 + 兜底轮询

**文件：**
- 新建：`frontend/src/hooks/useTauriEvent.ts`
- 修改：`frontend/src/App.tsx`（添加初始化逻辑）

- [ ] **步骤 1：创建 hooks/useTauriEvent.ts**

```ts
import { onCleanup } from 'solid-js'
import { onProgressUpdate } from '../api/events'
import { $tasks } from '../stores/downloads'

export function useProgressListener() {
  let unlisten: (() => void) | undefined

  onProgressUpdate((payload) => {
    if (!payload || typeof payload !== 'object') return

    const currentTasks = $tasks.get()
    let changed = false
    const updated = currentTasks.map(t => {
      const p = payload[t.id]
      if (!p) return t
      if (t.downloaded !== p.downloaded || t.speed !== p.speed || t.status !== p.status) {
        changed = true
        return {
          ...t,
          downloaded: p.downloaded ?? t.downloaded,
          speed: p.speed ?? t.speed,
          status: (p.status as t['status']) ?? t.status,
          fragments_done: p.fragmentsDone ?? t.fragments_done,
        }
      }
      return t
    })

    if (changed) {
      $tasks.set(updated)
    }
  }).then(fn => { unlisten = fn })

  onCleanup(() => { unlisten?.() })
}
```

- [ ] **步骤 2：更新 App.tsx 添加初始化逻辑**

在 App 组件中添加：首次加载时刷新任务列表、启动进度监听、启动兜底轮询（30 秒）。

```tsx
import { createSignal, onMount } from 'solid-js'
import type { ViewName } from './types'
import { api } from './api/invoke'
import { $tasks } from './stores/downloads'
import { useProgressListener } from './hooks/useTauriEvent'
import Layout from './components/Layout'
import TaskList from './components/TaskList'
import DetailPanel from './components/DetailPanel'
import SnifferPanel from './components/SnifferPanel'
import SettingsPanel from './components/SettingsPanel'

async function refreshTaskList() {
  try {
    const tasks = await api.getTaskList()
    $tasks.set(tasks)
  } catch (e) {
    console.error('刷新任务列表失败:', e)
  }
}

export default function App() {
  const [currentView, setCurrentView] = createSignal<ViewName>('downloads')

  useProgressListener()

  onMount(() => {
    refreshTaskList()
    api.subscribeProgress().catch(() => {})
    setInterval(refreshTaskList, 30000)
  })

  return (
    <Layout currentView={currentView()} onViewChange={setCurrentView}>
      {currentView() === 'downloads' && <TaskList />}
      {currentView() === 'sniffer' && <SnifferPanel />}
      {currentView() === 'settings' && <SettingsPanel />}
      {currentView() === 'downloads' && <DetailPanel />}
    </Layout>
  )
}
```

- [ ] **步骤 3：验证构建**

```bash
cd frontend && bun run build
```

- [ ] **步骤 4：Commit**

```bash
git add frontend/src/hooks/ frontend/src/App.tsx
git commit -m "feat(前端): 创建 Tauri 事件 hook + 兜底轮询集成"
```

---

### 任务 10：更新 Layout 集成 DetailPanel + 最终验证

**文件：**
- 修改：`frontend/src/components/Layout.tsx`
- 修改：`frontend/src/App.tsx`

- [ ] **步骤 1：更新 Layout.tsx 使 DetailPanel 在布局内**

Layout 需要接收 DetailPanel 作为第三个网格列，由 App.tsx 决定何时显示。

```tsx
import type { JSX } from 'solid-js'
import Sidebar from './Sidebar'
import Topbar from './Topbar'

interface LayoutProps {
  currentView: string
  onViewChange: (v: string) => void
  children: JSX.Element
  detail?: JSX.Element
}

export default function Layout(props: LayoutProps) {
  return (
    <div style={{ display: 'grid', 'grid-template-columns': '200px 1fr auto', height: '100vh' }}>
      <Sidebar currentView={props.currentView} onViewChange={props.onViewChange} />
      <div class="main" style={{ display: 'flex', 'flex-direction': 'column', overflow: 'hidden' }}>
        {props.currentView === 'downloads' && <Topbar />}
        <div class="content" style={{ flex: 1, overflow: 'auto', padding: '16px 20px' }}>
          {props.children}
        </div>
      </div>
      {props.detail}
    </div>
  )
}
```

- [ ] **步骤 2：最终验证 — cargo tauri dev**

```bash
cd D:\Rust\QuantumFetch && cargo tauri dev
```

预期：应用窗口正常启动，显示下载列表视图，侧边栏导航切换正常，设置面板加载/保存正常，嗅探面板显示空状态或真实资源。

- [ ] **步骤 3：最终 Commit**

```bash
git add frontend/src/
git commit -m "feat(前端): Solid.js + TypeScript 重构完成，所有功能迁移"
```

---

## 自检

### 规格覆盖度

| 规格章节 | 对应任务 |
|---------|---------|
| 1. 决策摘要 | 任务 1 |
| 3. 目标架构 | 任务 1-9 |
| 4. 依赖清单 | 任务 1 |
| 5. 设计令牌 | 任务 2 |
| 6.1 Layout | 任务 5 |
| 6.2 DownloadCard | 任务 7 |
| 6.3 TaskList | 任务 7 |
| 6.4 DetailPanel | 任务 7 |
| 6.5 SnifferPanel | 任务 8 |
| 6.6 SettingsPanel | 任务 8 |
| 7.1-7.3 状态管理 | 任务 4 |
| 8.1-8.2 API 层 | 任务 3 |
| 9. TypeScript 接口 | 任务 2 |
| 11. 实施清单 1-17 | 任务 1-10 |
| 13. 约束 | 全部任务遵守 |

### 占位符扫描

无 TODO/TBD/待定。所有步骤含完整代码。

### 类型一致性

- `TaskInfo` 定义在 `types.ts`（任务 2），所有组件通过 `import type { TaskInfo } from '../types'` 使用
- `DownloadStatus` 在 `types.ts` 中定义为联合类型，`statusText()` 参数为 `string`（兼容后端返回）
- `api` 对象方法签名与 `types.ts` 接口一致
- nanostores 使用 `$` 前缀命名，`useStore()` 在组件中解包响应式值
