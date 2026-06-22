interface HfUrlParseResult {
  repoId: string
  revision: string | null
}

/**
 * 解析 HuggingFace URL，提取 repo_id 和 revision。
 *
 * 支持的 URL 格式:
 * - https://huggingface.co/{owner}/{repo}
 * - https://huggingface.co/{owner}/{repo}/tree/{revision}
 * - https://huggingface.co/{owner}/{repo}/resolve/{revision}/{path}
 * - https://hf-mirror.com/{owner}/{repo} (镜像站点)
 */
export function parseHfUrl(url: string): HfUrlParseResult | null {
  try {
    const parsed = new URL(url)
    const allowedHosts = ['huggingface.co', 'hf-mirror.com']
    if (!allowedHosts.some((h) => parsed.hostname === h || parsed.hostname.endsWith(`.${h}`))) {
      return null
    }

    const pathParts = parsed.pathname.split('/').filter((p) => p.length > 0)
    if (pathParts.length < 2) return null

    const repoId = `${pathParts[0]}/${pathParts[1]}`
    let revision: string | null = null

    if (pathParts.length >= 4 && (pathParts[2] === 'tree' || pathParts[2] === 'resolve')) {
      revision = pathParts[3]
    }

    return { repoId, revision }
  } catch {
    return null
  }
}

/**
 * 判断输入是否是 repo_id 格式（owner/repo）。
 * 规则：包含 `/` 且两段都不为空。
 */
export function isRepoId(input: string): boolean {
  const parts = input.split('/')
  return parts.length === 2 && parts[0].length > 0 && parts[1].length > 0
}
