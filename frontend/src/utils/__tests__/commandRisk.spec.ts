import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import type { Mock } from 'vitest'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { COMMAND_RISK, getRiskTier, confirmDestructive } from '../commandRisk'

describe('commandRisk (P1-11a)', () => {
  describe('COMMAND_RISK 风险表覆盖', () => {
    it('safe 级命令全部登记', () => {
      const safe = Object.entries(COMMAND_RISK)
        .filter(([, tier]) => tier === 'safe')
        .map(([cmd]) => cmd)
      // 至少包含核心只读命令
      const expected = [
        'get_app_info',
        'supported_protocols',
        'get_task_list',
        'get_task_detail',
        'get_download_progress',
        'subscribe_progress',
        'get_sniffer_resources',
        'get_config',
        'list_repo_files',
        'get_hf_download_url',
        // B6 补登:Hub/模型查询类只读命令
        'get_task_fragments',
        'get_model_info',
        'search_models',
        'scan_local_models',
        'verify_model',
        'list_model_favorites',
      ]
      for (const cmd of expected) {
        expect(safe).toContain(cmd)
      }
    })

    it('mutate 级命令全部登记', () => {
      const mutate = Object.entries(COMMAND_RISK)
        .filter(([, tier]) => tier === 'mutate')
        .map(([cmd]) => cmd)
      const expected = [
        'pause_task',
        'resume_task',
        'cancel_task',
        'add_sniffer_filter',
        'create_task',
        // B6 补登:模型收藏与 HF 批量任务写操作
        'add_model_favorite',
        'remove_model_favorite',
        'batch_create_hf_tasks',
      ]
      for (const cmd of expected) {
        expect(mutate).toContain(cmd)
      }
    })

    it('destructive 级命令全部登记', () => {
      const destructive = Object.entries(COMMAND_RISK)
        .filter(([, tier]) => tier === 'destructive')
        .map(([cmd]) => cmd)
      expect(destructive).toContain('delete_task')
      expect(destructive).toContain('update_config')
      expect(destructive).toContain('import_backup')
      expect(destructive).toContain('export_backup')
    })

    it('风险表至少覆盖 15 个命令(无遗漏)', () => {
      // invoke.ts 暴露的命令数
      expect(Object.keys(COMMAND_RISK).length).toBeGreaterThanOrEqual(15)
    })

    it('B6 补登:9 条新增命令 getRiskTier 返回正确等级', () => {
      // 查询类(只读校验,无副作用)→ safe
      expect(getRiskTier('get_task_fragments')).toBe('safe')
      expect(getRiskTier('get_model_info')).toBe('safe')
      expect(getRiskTier('search_models')).toBe('safe')
      expect(getRiskTier('scan_local_models')).toBe('safe')
      expect(getRiskTier('verify_model')).toBe('safe')
      expect(getRiskTier('list_model_favorites')).toBe('safe')
      // 写操作(状态变更/网络触发)→ mutate
      expect(getRiskTier('add_model_favorite')).toBe('mutate')
      expect(getRiskTier('remove_model_favorite')).toBe('mutate')
      expect(getRiskTier('batch_create_hf_tasks')).toBe('mutate')
    })

    it('B6 补登后不再误判为 destructive(避免查询类误弹确认)', () => {
      // 回归:get_task_fragments 在 DetailPanel 打开时调用,不得被判为 destructive
      for (const cmd of [
        'get_task_fragments',
        'get_model_info',
        'search_models',
        'scan_local_models',
        'verify_model',
        'list_model_favorites',
      ]) {
        expect(getRiskTier(cmd)).not.toBe('destructive')
      }
    })
  })

  describe('getRiskTier', () => {
    it('已登记命令返回对应风险等级', () => {
      expect(getRiskTier('delete_task')).toBe('destructive')
      expect(getRiskTier('get_task_list')).toBe('safe')
      expect(getRiskTier('pause_task')).toBe('mutate')
    })

    it('未登记命令默认 destructive(白名单原则)', () => {
      expect(getRiskTier('unknown_evil_command')).toBe('destructive')
    })

    it('invoke.ts 暴露的全部命令均已登记风险等级(防未登记误弹确认)', () => {
      // 回归:get_quic_capability 等 12 条命令曾未登记,默认 destructive
      // 导致切到「连接」tab / 加标签 / 拖拽排序时误弹破坏性确认框
      const invokeSrc = readFileSync(
        resolve(process.cwd(), 'src/api/invoke.ts'),
        'utf-8',
      )
      const cmds = new Set<string>()
      // 匹配 invoke<T>('cmd_name' 及 fn<T>('cmd_name'(requestConfirmation 直调)
      for (const m of invokeSrc.matchAll(
        /(?:invoke|fn)<[^>(]*>\(\s*'([a-z_]+)'/g,
      )) {
        cmds.add(m[1]!)
      }
      expect(cmds.size).toBeGreaterThan(10)
      for (const cmd of cmds) {
        expect(
          COMMAND_RISK[cmd],
          `命令 ${cmd} 在 invoke.ts 中暴露但未登记风险等级`,
        ).toBeDefined()
      }
    })

    it('补登命令分级正确:查询类 safe,状态变更类 mutate', () => {
      // 只读查询(切「连接」tab 即调用,误弹确认的元凶)
      expect(getRiskTier('get_quic_capability')).toBe('safe')
      expect(getRiskTier('get_bt_proxy_coverage')).toBe('safe')
      expect(getRiskTier('get_sniffer_capture_config')).toBe('safe')
      // 可恢复状态变更
      for (const cmd of [
        'add_sniffer_resource',
        'clear_sniffer_resources',
        'create_task_from_sniffer',
        'set_sniffer_capture_config',
        'add_task_tag',
        'remove_task_tag',
        'set_task_tags',
        'move_task',
        'reorder_tasks',
      ]) {
        expect(getRiskTier(cmd), `${cmd} 应为 mutate`).toBe('mutate')
      }
    })
  })

  describe('confirmDestructive', () => {
    beforeEach(() => {
      vi.stubGlobal('confirm', vi.fn(() => true))
    })

    afterEach(() => {
      vi.unstubAllGlobals()
    })

    it('safe 命令直接放行,不弹确认', async () => {
      const result = await confirmDestructive('get_task_list')
      expect(result).toBe(true)
      expect(window.confirm).not.toHaveBeenCalled()
    })

    it('mutate 命令直接放行,不弹确认', async () => {
      const result = await confirmDestructive('create_task')
      expect(result).toBe(true)
      expect(window.confirm).not.toHaveBeenCalled()
    })

    it('destructive 命令用户确认时返回 true', async () => {
      vi.stubGlobal('confirm', vi.fn(() => true))
      const result = await confirmDestructive('delete_task')
      expect(result).toBe(true)
      expect(window.confirm).toHaveBeenCalledTimes(1)
      // 确认对话框应包含描述文本
      const firstCall = (window.confirm as Mock).mock.calls[0]
      expect(firstCall).toBeDefined()
      const callArg = firstCall![0] as string
      expect(callArg).toContain('删除')
    })

    it('destructive 命令用户取消时返回 false', async () => {
      vi.stubGlobal('confirm', vi.fn(() => false))
      const result = await confirmDestructive('delete_task')
      expect(result).toBe(false)
    })

    it('update_config 确认对话框含"配置"描述', async () => {
      vi.stubGlobal('confirm', vi.fn(() => true))
      await confirmDestructive('update_config')
      const firstCall = (window.confirm as Mock).mock.calls[0]
      expect(firstCall).toBeDefined()
      const callArg = firstCall![0] as string
      expect(callArg).toContain('配置')
    })

    it('import_backup 确认对话框含"导入"描述', async () => {
      vi.stubGlobal('confirm', vi.fn(() => true))
      await confirmDestructive('import_backup')
      const firstCall = (window.confirm as Mock).mock.calls[0]
      expect(firstCall).toBeDefined()
      const callArg = firstCall![0] as string
      expect(callArg).toContain('导入')
    })

    it('未登记命令视为 destructive,弹确认', async () => {
      vi.stubGlobal('confirm', vi.fn(() => true))
      const result = await confirmDestructive('unknown_command')
      expect(result).toBe(true)
      expect(window.confirm).toHaveBeenCalledTimes(1)
    })
  })
})
