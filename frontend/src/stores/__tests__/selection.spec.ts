import { describe, it, expect, beforeEach } from 'vitest'
import {
  toggleSelection,
  selectAll,
  deselectAll,
  selectRange,
  isSelected,
  selectedCount,
  getLastSelectedAnchorId,
} from '../selection'

describe('selection store', () => {
  beforeEach(() => {
    deselectAll()
  })

  it('toggleSelection 切换选中状态并记录锚点', () => {
    toggleSelection('a')
    expect(isSelected('a')).toBe(true)
    expect(getLastSelectedAnchorId()).toBe('a')

    toggleSelection('a')
    expect(isSelected('a')).toBe(false)
    expect(getLastSelectedAnchorId()).toBe('a')
  })

  it('selectAll 选中全部并设锚点为最后一项', () => {
    selectAll(['a', 'b', 'c'])
    expect(selectedCount()).toBe(3)
    expect(getLastSelectedAnchorId()).toBe('c')
  })

  it('deselectAll 清空选区与锚点', () => {
    selectAll(['a', 'b'])
    deselectAll()
    expect(selectedCount()).toBe(0)
    expect(getLastSelectedAnchorId()).toBeNull()
  })

  it('selectRange 从锚点向后连选', () => {
    toggleSelection('a')
    selectRange('a', 'd', ['a', 'b', 'c', 'd', 'e'])
    expect(isSelected('a')).toBe(true)
    expect(isSelected('b')).toBe(true)
    expect(isSelected('c')).toBe(true)
    expect(isSelected('d')).toBe(true)
    expect(isSelected('e')).toBe(false)
    expect(getLastSelectedAnchorId()).toBe('a')
  })

  it('selectRange 从锚点向前连选', () => {
    toggleSelection('d')
    selectRange('d', 'b', ['a', 'b', 'c', 'd', 'e'])
    expect(isSelected('a')).toBe(false)
    expect(isSelected('b')).toBe(true)
    expect(isSelected('c')).toBe(true)
    expect(isSelected('d')).toBe(true)
    expect(isSelected('e')).toBe(false)
  })

  it('selectRange 保留已有选区并扩展范围', () => {
    selectAll(['a', 'e'])
    selectRange('a', 'c', ['a', 'b', 'c', 'd', 'e'])
    expect(isSelected('a')).toBe(true)
    expect(isSelected('b')).toBe(true)
    expect(isSelected('c')).toBe(true)
    expect(isSelected('d')).toBe(false)
    expect(isSelected('e')).toBe(true)
  })

  it('selectRange 对不存在的锚点或终点无操作', () => {
    selectAll(['a'])
    selectRange('x', 'c', ['a', 'b', 'c'])
    expect(selectedCount()).toBe(1)
    selectRange('a', 'x', ['a', 'b', 'c'])
    expect(selectedCount()).toBe(1)
  })
})
