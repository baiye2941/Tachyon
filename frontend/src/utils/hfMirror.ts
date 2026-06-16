/**
 * HuggingFace 镜像 URL 构造(Iteration 06,PF-3)。
 *
 * 旧实现用 `originalUrl.replace('huggingface.co', 'hf-mirror.com')`,但 HF 大文件
 * 的 LFS/CDN 下载链接形如 `https://cdn-lfs.huggingface.co/...` 或新 Xet 架构的
 * `https://cas-bridge.xethub.hf.co/...`——简单 replace 对 CDN 子域漏匹配,
 * 对 Xet 域名(无 `huggingface.co` 子串)完全无效,导致镜像下载静默回退原始 HF
 * 链接(用户以为走了镜像,实际没走,国内加速失效)。
 *
 * 正确做法:基于 repoId 构造 hf-mirror.com 的 resolve 端点 URL。
 * hf-mirror 内部处理 LFS/Xet 重定向,对普通文件、LFS、Xet 统一生效。
 */

/**
 * 构造 hf-mirror.com 镜像下载 URL。
 *
 * 规则:`https://hf-mirror.com/{repoId}/resolve/{revision}/{filePath}`
 *
 * 安全性:repoId/revision/filePath 已由后端 `validate_repo_id`/`validate_revision`/
 * `validate_file_path` 校验(禁止 `..`、绝对路径、非法字符),前端构造不会注入
 * 路径遍历。filePath 各段用 encodeURIComponent 编码,处理空格/中文等特殊字符,
 * 避免 URL 解析歧义(repoId/revision 已被后端校验为安全字符集,不编码)。
 *
 * @param repoId    HuggingFace 仓库 ID,格式 `owner/repo`
 * @param revision  分支/tag,默认 `main`
 * @param filePath  仓库内相对文件路径
 * @returns hf-mirror.com 的 resolve 下载 URL
 */
export function buildHfMirrorUrl(repoId: string, revision: string, filePath: string): string {
  const rev = revision || 'main'
  const encodedPath = filePath
    .split('/')
    .map((seg) => encodeURIComponent(seg))
    .join('/')
  return `https://hf-mirror.com/${repoId}/resolve/${rev}/${encodedPath}`
}
