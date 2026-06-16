/**
 * HuggingFace 仓库文件树构建。
 *
 * 从后端 `list_repo_files` 返回的扁平文件列表(可能含 directory 节点)
 * 构建为前端可渲染的树结构。
 *
 * 契约对齐(Iteration 06,PF-1):
 *   后端 `HfFile.file_type` 返回 "file" | "directory"。HF `/tree?recursive=true`
 *   端点本身返回 directory 节点。本函数尊重 type 字段:directory 节点直接登记
 *   为目录(不作为叶子渲染),file 节点沿 path 建父目录。
 *
 * 幂等性:无论后端是否返回 directory 节点(有些仓库只返回扁平 file 列表),
 *   都能正确建树——rootMap 按 path 索引,重复 ensureDir 不创建重复目录。
 */

import type { HubFileInfo } from '../types'

/** 树节点 */
export interface TreeNode {
  name: string
  path: string
  isDirectory: boolean
  size: number | null
  lfs?: { oid: string; size: number } | null
  children: TreeNode[]
}

/** 100MB,与 modelMeta.isLargeFile 阈值对齐 */
const LARGE_FILE_THRESHOLD = 100 * 1024 * 1024

/**
 * 将扁平文件列表构建为树结构。
 *
 * 复杂度:O(n) 遍历 + O(n log n) 排序。排序采用**原地**(sortInPlace),
 * 不递归创建全新对象树(避免内存翻倍与引用不稳定)。
 *
 * 设计决策:
 * - 用 rootMap(path → node)索引,父目录查找 O(1)。
 * - directory 节点 `continue`,绝不 push 成叶子——根治「目录被当文件」。
 * - 排序目录优先 + 名称 localeCompare,保持与 HF Web UI 一致的呈现。
 */
export function buildTree(files: HubFileInfo[]): TreeNode[] {
  const rootMap = new Map<string, TreeNode>()
  const root: TreeNode[] = []

  /** 登记目录(若已存在则复用),返回该目录节点 */
  const ensureDir = (dirPath: string, name: string): TreeNode => {
    const existing = rootMap.get(dirPath)
    if (existing) return existing
    const node: TreeNode = {
      name,
      path: dirPath,
      isDirectory: true,
      size: null,
      lfs: null,
      children: [],
    }
    rootMap.set(dirPath, node)
    return node
  }

  for (const file of files) {
    const parts = file.path.split('/')
    const lastIdx = parts.length - 1
    const leafName = parts[lastIdx]!

    // 沿 path 建父目录链(目录优先 + 挂载到对应父级 children)。
    // directory 与 file 节点共用此步骤,确保纯 directory 节点(无子文件)
    // 也能正确挂到树中——这是 ensureDir-only 方案的缺陷修复。
    let parentChildren = root
    for (let i = 0; i < lastIdx; i++) {
      const dirPath = parts.slice(0, i + 1).join('/')
      const dirName = parts[i]!
      let dirNode = rootMap.get(dirPath)
      if (!dirNode) {
        dirNode = ensureDir(dirPath, dirName)
        parentChildren.push(dirNode)
      }
      parentChildren = dirNode.children
    }

    // 叶子处理:directory 登记为目录(并挂到父级,若尚未挂载);
    // file 挂为叶子
    if (file.type === 'directory') {
      let dirNode = rootMap.get(file.path)
      if (!dirNode) {
        dirNode = ensureDir(file.path, leafName)
        parentChildren.push(dirNode)
      }
      continue
    }

    parentChildren.push({
      name: leafName,
      path: file.path,
      isDirectory: false,
      size: file.size,
      lfs: file.lfs,
      children: [],
    })
  }

  // 原地排序:目录优先 + 名称序(递归子层)
  const sortInPlace = (nodes: TreeNode[]): void => {
    nodes.sort((a, b) =>
      a.isDirectory !== b.isDirectory
        ? a.isDirectory
          ? -1
          : 1
        : a.name.localeCompare(b.name),
    )
    for (const n of nodes) {
      if (n.children.length > 0) sortInPlace(n.children)
    }
  }
  sortInPlace(root)

  return root
}

/**
 * 扁平化树节点(用于筛选/计数场景)。
 *
 * 仅返回文件节点(排除目录),保留 path 便于匹配。
 * 与 HfBrowserPanel 的 fileCount 过滤逻辑一致。
 */
export function flattenFiles(files: HubFileInfo[]): HubFileInfo[] {
  return files.filter((f) => f.type !== 'directory')
}

/** 文件类型计数(用于筛选条徽章) */
export interface FileCounts {
  all: number
  gguf: number
  safetensors: number
  large: number
}

/** 计算文件类型计数 */
export function countByType(files: HubFileInfo[]): FileCounts {
  let gguf = 0
  let safetensors = 0
  let large = 0
  let all = 0
  for (const f of files) {
    if (f.type === 'directory') continue
    all++
    const lower = f.path.toLowerCase()
    if (lower.endsWith('.gguf')) gguf++
    if (lower.endsWith('.safetensors')) safetensors++
    if (f.size >= LARGE_FILE_THRESHOLD) large++
  }
  return { all, gguf, safetensors, large }
}
