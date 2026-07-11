/**
 * 将路径中的分隔符还原为原始风格。
 *
 * @param path 已使用正斜杠处理过的路径
 * @param useWindows 是否使用 Windows 反斜杠风格
 */
function toOriginalSeparators(path: string, useWindows: boolean): string {
  return useWindows ? path.replace(/\//g, '\\') : path
}

/**
 * 获取路径对应的父目录。
 *
 * 语义约定：
 * - 空路径返回空字符串；
 * - 以分隔符结尾的路径视为目录路径，返回去除末尾分隔符后的目录；
 * - 否则视为文件路径，返回其所在目录；
 * - 无分隔符的相对文件名返回 `"."`；
 * - 根路径（`/`、`C:\\` 等）返回自身。
 *
 * 该函数仅做字符串处理，不访问文件系统，兼容 Windows（`\\`）与 POSIX（`/`）分隔符。
 */
export function getParentDirectory(path: string): string {
  if (!path) return ''

  const useWindowsSep = path.includes('\\')
  let normalized = path.replace(/\\/g, '/')

  // 根路径直接返回自身
  if (normalized === '/') return '/'
  if (/^[a-zA-Z]:\/$/.test(normalized)) return path

  // 目录路径以分隔符结尾，去除末尾分隔符后视为已指定的目录
  const isDirectoryPath = normalized.endsWith('/')
  normalized = normalized.replace(/\/+$/, '')

  // 仅余下分隔符的视作根路径
  if (normalized === '') return useWindowsSep ? '\\' : '/'
  // 裸盘符返回自身
  if (/^[a-zA-Z]:$/.test(normalized)) return path
  // UNC 共享根目录（如 \\server\share）返回自身
  if (/^\/\/[^/]+\/[^/]+$/.test(normalized)) {
    return toOriginalSeparators(normalized, useWindowsSep)
  }

  if (isDirectoryPath) {
    return toOriginalSeparators(normalized, useWindowsSep)
  }

  const lastSep = normalized.lastIndexOf('/')
  if (lastSep === -1) return '.'
  if (lastSep === 0) return '/'

  let parent = normalized.slice(0, lastSep)
  // 父目录退到盘符根时补回分隔符，如 C:/file -> C:/
  if (/^[a-zA-Z]:$/.test(parent)) {
    parent += '/'
  }
  return toOriginalSeparators(parent, useWindowsSep)
}
