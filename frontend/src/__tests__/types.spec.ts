import { describe, it, expect, expectTypeOf } from 'vitest'
import type { TaskInfo } from '../types'

describe('TaskInfo types (E-08)', () => {
  it('声明 mirrorUrls 可选字段以对齐后端 TaskInfo.mirror_urls', () => {
    const task = {
      id: 't1',
      url: 'https://example.com/a.bin',
      fileName: 'a.bin',
      fileSize: 1,
      downloaded: 0,
      speed: 0,
      status: 'downloading',
      progress: 0,
      fragmentsTotal: 1,
      fragmentsDone: 0,
      createdAt: '2026-01-01T00:00:00Z',
      savePath: '/tmp/a.bin',
      mirrorUrls: ['https://mirror.example.com/a.bin'],
    } satisfies TaskInfo

    expect(task.mirrorUrls?.[0]).toContain('mirror.example.com')
    expectTypeOf<TaskInfo>().toHaveProperty('mirrorUrls')
  })
})
