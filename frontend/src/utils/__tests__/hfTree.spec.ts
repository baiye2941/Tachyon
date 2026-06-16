import { describe, it, expect } from 'vitest'
import { buildTree, flattenFiles, countByType } from '../hfTree'
import type { HubFileInfo } from '../../types'

const file = (path: string, overrides: Partial<HubFileInfo> = {}): HubFileInfo => ({
  type: 'file',
  path,
  size: 1024,
  ...overrides,
})

const dir = (path: string): HubFileInfo => ({
  type: 'directory',
  path,
  size: 0,
})

/** 在树中按 path 查找节点 */
const findNode = (tree: ReturnType<typeof buildTree>, path: string) => {
  const visit = (nodes: typeof tree): ReturnType<typeof buildTree>[number] | undefined => {
    for (const n of nodes) {
      if (n.path === path) return n
      if (n.children.length) {
        const found = visit(n.children)
        if (found) return found
      }
    }
    return undefined
  }
  return visit(tree)
}

describe('buildTree — 契约对齐(Iteration 06 PF-1)', () => {
  it('后端返回含 directory 节点时,目录不被当文件渲染', () => {
    const tree = buildTree([
      dir('foo'),
      dir('foo/bar'),
      file('foo/bar/model.gguf', { size: 4096 }),
    ])

    // foo 应为目录,有子节点,非叶子
    const foo = findNode(tree, 'foo')
    expect(foo).toBeDefined()
    expect(foo!.isDirectory).toBe(true)
    expect(foo!.children.length).toBe(1) // bar

    // bar 应为 foo 的子目录
    const bar = findNode(tree, 'foo/bar')
    expect(bar).toBeDefined()
    expect(bar!.isDirectory).toBe(true)

    // model.gguf 应为 bar 的子文件
    const model = findNode(tree, 'foo/bar/model.gguf')
    expect(model).toBeDefined()
    expect(model!.isDirectory).toBe(false)
    expect(model!.size).toBe(4096)

    // 根级不应出现名为 "foo" 的文件叶子(directory 不应被 push 成文件)
    const rootFileNamedFoo = tree.find((n) => n.name === 'foo' && !n.isDirectory)
    expect(rootFileNamedFoo).toBeUndefined()
  })

  it('后端只返回扁平 file 节点时,沿 path 正确建父目录', () => {
    const tree = buildTree([
      file('a/b/c.gguf'),
      file('config.json'),
      file('README.md'),
    ])

    expect(tree.length).toBe(3) // a/(dir), config.json, README.md
    const aDir = tree.find((n) => n.name === 'a')
    expect(aDir?.isDirectory).toBe(true)
    const bDir = findNode(tree, 'a/b')
    expect(bDir?.isDirectory).toBe(true)
    const cFile = findNode(tree, 'a/b/c.gguf')
    expect(cFile?.isDirectory).toBe(false)

    // 根级文件 config.json / README.md 存在
    expect(tree.find((n) => n.name === 'config.json')).toBeDefined()
    expect(tree.find((n) => n.name === 'README.md')).toBeDefined()
  })

  it('重复 path(directory + file 同 path 片段)幂等,不创建重复目录', () => {
    // 后端同时返回 directory "foo" 和 file "foo/x.gguf",且 file 的 path 也隐含 foo/
    const tree = buildTree([dir('foo'), file('foo/x.gguf')])
    const foo = findNode(tree, 'foo')
    expect(foo).toBeDefined()
    expect(foo!.isDirectory).toBe(true)
    // 只有一个 foo 目录,无重复
    const fooCount = tree.filter((n) => n.name === 'foo').length
    expect(fooCount).toBe(1)
  })

  it('排序:目录优先 + 名称序', () => {
    const tree = buildTree([
      file('zebra.gguf'),
      dir('alpha'),
      file('apple.gguf'),
      dir('beta'),
    ])
    const names = tree.map((n) => n.name)
    // 目录优先:alpha, beta;然后文件:apple.gguf, zebra.gguf
    expect(names).toEqual(['alpha', 'beta', 'apple.gguf', 'zebra.gguf'])
  })

  it('递归子层也按目录优先 + 名称序', () => {
    const tree = buildTree([
      file('dir/z-file.gguf'),
      file('dir/a-file.gguf'),
      file('dir/sub/y.gguf'),
      file('dir/sub/x.gguf'),
    ])
    const dirNode = findNode(tree, 'dir')!
    expect(dirNode.children.map((n) => n.name)).toEqual(['sub', 'a-file.gguf', 'z-file.gguf'])
    const subNode = findNode(tree, 'dir/sub')!
    expect(subNode.children.map((n) => n.name)).toEqual(['x.gguf', 'y.gguf'])
  })

  it('LFS 信息透传到文件节点', () => {
    const lfs = { oid: 'sha256:abc', size: 9999 }
    const tree = buildTree([file('model.safetensors', { size: 2048, lfs })])
    const node = tree[0]!
    expect(node.lfs).toEqual(lfs)
  })

  it('空输入返回空树', () => {
    expect(buildTree([])).toEqual([])
  })
})

describe('flattenFiles', () => {
  it('排除 directory 节点', () => {
    const files = [dir('foo'), file('foo/a.gguf'), file('b.txt')]
    expect(flattenFiles(files).length).toBe(2)
  })
})

describe('countByType', () => {
  it('正确统计各类型计数', () => {
    const files: HubFileInfo[] = [
      file('a.gguf', { size: 200 * 1024 * 1024 }), // gguf + large
      file('b.safetensors', { size: 50 * 1024 * 1024 }), // safetensors
      file('c.gguf', { size: 1024 }), // gguf
      file('d.txt', { size: 10 }), // other
      dir('subdir'), // 排除
    ]
    const counts = countByType(files)
    expect(counts).toEqual({ all: 4, gguf: 2, safetensors: 1, large: 1 })
  })

  it('空列表返回全零', () => {
    expect(countByType([])).toEqual({ all: 0, gguf: 0, safetensors: 0, large: 0 })
  })
})
