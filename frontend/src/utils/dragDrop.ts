/**
 * 拖拽文件解析工具。
 *
 * 处理用户从文件管理器拖入的文件,提取其中的下载链接。
 * 当前支持 .txt 文件(逐行解析 URL),未来可扩展 .torrent/.metalink。
 *
 * 实现说明:
 * - 使用浏览器 File API(File.text()),Tauri Webview2 支持。
 * - 不依赖 @tauri-apps/plugin-fs,避免引入新依赖。
 * - 文件路径在 Webview 沙箱中不可用,仅读取内容。
 */

/**
 * 从拖拽的文件列表中解析出 URL。
 *
 * @param files 拖拽事件中的 FileList(DataTransfer.files)
 * @returns 解析出的 URL 数组(已去重、去空、去注释行)
 */
export async function parseDroppedFiles(
  files: FileList | null | undefined,
): Promise<string[]> {
  if (!files || files.length === 0) return []

  const urls: string[] = []
  for (const file of Array.from(files)) {
    const ext = file.name.split('.').pop()?.toLowerCase()

    if (ext === 'txt') {
      try {
        const text = await file.text()
        const lines = text
          .split(/\r?\n/)
          .map((s) => s.trim())
          .filter((s) => s.length > 0 && !s.startsWith('#'))
        urls.push(...lines)
      } catch {
        // 读取失败则跳过该文件
      }
    }
    // 未来扩展:
    // - .torrent: 需后端 BT 协议支持
    // - .metalink: 需后端 metalink 解析
  }

  // 去重
  return Array.from(new Set(urls))
}
