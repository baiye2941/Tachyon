/**
 * Fuzzy 子序列搜索 + 评分(Iteration 07,DI-4)。
 *
 * 替代 CommandPalette 原有的 `includes` 子串匹配。子序列匹配允许
 * query 字符在 target 中非连续出现(如 "pa" 匹配 "全部暂停" 的 P**a**use-all,
 * "nav" 匹配 "导航"),并通过评分排序——前缀匹配、连续匹配、词首匹配得分更高。
 *
 * 参考:Raycast/Linear 命令面板的 fuzzy 实现。算法为标准子序列匹配 +
 * 启发式加权(O(n×m),n=target 长度,m=query 长度,均短)。
 */

/** 单项匹配结果 */
export interface FuzzyResult<T> {
  item: T
  /** 分数,越高越匹配。-1 表示不匹配(不应出现在结果中) */
  score: number
  /** target 中匹配字符的索引集合(用于 UI 高亮) */
  matchedIndices: number[]
}

/**
 * 判断 query 是否为 target 的子序列,返回匹配分数与索引。
 *
 * @returns score = -1 表示不匹配;否则返回正分(越高越好)
 */
export function fuzzyMatch(query: string, target: string): { score: number; indices: number[] } {
  const q = query.toLowerCase()
  const t = target.toLowerCase()
  if (q.length === 0) return { score: 0, indices: [] }
  if (q.length > t.length) return { score: -1, indices: [] }

  const indices: number[] = []
  let ti = 0
  for (let qi = 0; qi < q.length; qi++) {
    const qc = q[qi]!
    // 跳过 target 中的空格做模糊匹配(可选,提升体验)
    let found = -1
    while (ti < t.length) {
      if (t[ti] === qc) {
        found = ti
        ti++
        break
      }
      ti++
    }
    if (found === -1) return { score: -1, indices: [] }
    indices.push(found)
  }

  // 评分启发式:
  // - 基础分:每个匹配字符 1 分
  // - 前缀匹配(query 第一个字符 = target 第一个字符):+20
  // - 连续匹配(相邻 index 差 1):每段额外 +15
  // - 词首匹配(index 0 或前一个字符是空格/标点):+10
  let score = indices.length
  if (indices[0] === 0) score += 20
  for (let i = 1; i < indices.length; i++) {
    if (indices[i]! - indices[i - 1]! === 1) score += 15 // 连续
  }
  for (const idx of indices) {
    if (idx === 0) score += 10 // 词首(已在前缀加分,叠加无妨)
    else {
      const prev = t[idx - 1]
      if (prev === ' ' || prev === '/' || prev === '-' || prev === '_') score += 10
    }
  }
  // 惩罚:target 越长(匹配越分散),分数略降
  score -= Math.floor((t.length - indices[indices.length - 1]!) / 10)

  return { score, indices }
}

/**
 * 对 items 做 fuzzy 搜索,过滤 + 按 score 降序排序。
 *
 * @param items 待搜索项
 * @param query 查询串(空则返回全部,score=0,保持原序)
 * @param getText 提取每项的搜索文本(通常是 label + hint)
 */
export function fuzzySearch<T>(
  items: T[],
  query: string,
  getText: (item: T) => string,
): FuzzyResult<T>[] {
  const q = query.trim()
  if (q.length === 0) {
    return items.map((item) => ({ item, score: 0, matchedIndices: [] }))
  }
  const results: FuzzyResult<T>[] = []
  for (const item of items) {
    const { score, indices } = fuzzyMatch(q, getText(item))
    if (score >= 0) results.push({ item, score, matchedIndices: indices })
  }
  // 稳定排序:score 降序,同分保持原序
  results.sort((a, b) => b.score - a.score)
  return results
}
