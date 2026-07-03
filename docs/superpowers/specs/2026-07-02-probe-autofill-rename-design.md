# 设计:新建任务探测后自动填名

- 日期: 2026-07-02
- 范围: `frontend/src/components/NewTaskModal.tsx`(单文件,约 4 处改动)
- 方案: 方案 A — 最小改动,复用现有 `probedFilename` signal

## 1. 问题与目标

### 问题

当前 NewTaskModal 里点"探测"按钮后,探测到的文件名只显示在上方的 `displayFilename` span(`NewTaskModal.tsx:474`),**没有**填入下方重命名 input 的 value。提交时(`NewTaskModal.tsx:186`)

```ts
const name = validCount() === 1 ? fileName().trim() || undefined : undefined;
```

`fileName()` 为空,取到 `undefined`,探测名根本没传给后端。任务创建后用的是后端 `probe_and_save_metadata` 二次探测的结果或 URL 提取名 —— 用户在对话框里的探测动作形同虚设。

### 目标

点探测后,探测名自动填入重命名 input 的 value(用户可继续编辑);提交时该值生效。同时处理批量、URL 变化、重新探测三个边界:

1. 填入 input value(可编辑):探测名写入 `fileName` signal,input 的 value 跟随,用户可继续改
2. 批量 URL 场景:批量(>1)时禁用探测按钮(与重命名 input 仅单 URL 显示对齐)
3. URL 变化时清空 input:URL 变化不仅清 `probedFilename`,也清 `fileName`,避免旧名与新 URL 错配
4. 重新探测覆盖用户输入:用户手动改过 input 后又点探测,用新探测名覆盖(符合用户明确选择)

## 2. 改动点

全部改动集中在 `frontend/src/components/NewTaskModal.tsx` 单文件,共 4 处。后端(`probe_filename` command、`create_task`、`TaskService`)完全不动。

### 改动 1 — `handleProbe` 成功后同步填入 input(98-111 行)

```ts
const handleProbe = async () => {
  const url = validUrls()[0];
  if (!url) return;
  setProbing(true);
  try {
    const name = await api.probeFilename(url);
    setProbedFilename(name);
    setFileName(name); // 新增:探测名填入重命名 input
  } catch {
    // 探测失败保持本地提取结果
  } finally {
    setProbing(false);
  }
};
```

满足"填入 input value(可编辑)"+"重新探测覆盖用户输入":每次探测成功都 `setFileName(name)`,即使用户之前手动改过也会被覆盖。

### 改动 2 — URL 变化时清空 input(59-63 行)

```ts
createEffect(() => {
  validUrls();
  setProbedFilename(null);
  setFileName(""); // 新增:URL 变化清空已填入的文件名
});
```

满足边界 2:URL 变了,旧探测名/手动输入大概率不匹配,清空避免错配。

### 改动 3 — 批量时禁用探测按钮(477-486 行)

```tsx
<Button
  variant="ghost"
  size="sm"
  loading={probing()}
  disabled={probing() || validCount() !== 1} // 改:=== 0 → !== 1
  onClick={handleProbe}
>
```

满足边界 1:批量(>1)时探测禁用。重命名 input 本就只在 `validCount() === 1` 显示(561 行),探测按钮与之对齐。

### 改动 4 — input placeholder 用 displayFilename(578-580 行)

```tsx
placeholder={
  displayFilename() || tr("newTask.fileNamePlaceholder") // 改:suggestedFileName → displayFilename
}
```

视觉一致性:探测后即便用户清空了 input,placeholder 仍显示探测名作为提示。无功能影响,纯体验优化。

### 不改的部分

- `displayFilename` memo(92-96 行)逻辑不变,继续驱动上方 span 显示
- `handleSubmit`(185-186 行)`fileName().trim() || undefined` 不变 —— 现在探测后 `fileName` 有值,会正确传给后端
- 后端 `probe_filename` command / `create_task` 完全不动
- i18n 无需新增 key(探测按钮文案 `newTask.probeFilename` 已存在)

## 3. 数据流

```
用户点探测按钮
  → handleProbe()
  → api.probeFilename(url)        // Tauri command,返回 string
  → setProbedFilename(name)       // 驱动上方 displayFilename span
  → setFileName(name)             // [新增] 驱动下方重命名 input value
  → input 显示探测名(可编辑)
  → 用户提交
  → handleSubmit()
  → fileName().trim() = 探测名(或用户编辑后的值)
  → api.createTask(url, dir, mirrors, name)  // name 传给后端
```

URL 变化时:

```
urlText 变化 → validUrls() 变化
  → createEffect 触发
  → setProbedFilename(null)
  → setFileName("")               // [新增] 清空 input
```

## 4. 测试与验证

### 单元测试

新增 `frontend/src/components/__tests__/NewTaskModal.probe-autofill.spec.tsx`,用 `@solidjs/testing-library` + `vi`:

1. **探测成功后 input value 被填充**:mock `api.probeFilename` 返回 `"model.safetensors"`,展开高级选项(重命名 input 在 `advancedOpen` 的 Show 内),点击探测按钮,断言重命名 input 的 value === `"model.safetensors"`
2. **探测后可继续编辑**:上一步基础上,在 input 触发 `onInput` 改成 `"model-renamed.safetensors"`,断言 value 跟随
3. **URL 变化清空 input**:先探测填充,再改 urlText 触发 `validUrls` 变化,断言 input value === `""`
4. **重新探测覆盖用户输入**:先手动输入 `"old.bin"`,再点探测返回 `"new.bin"`,断言 value === `"new.bin"`
5. **批量时探测按钮禁用**:urlText 填两个 URL,断言探测按钮 `disabled === true`

### 手动验证

- `cd frontend && bun run dev` + `cargo tauri dev`
- 粘贴一个真实 HTTP URL,点探测,确认 input 自动填名
- 编辑 input,提交,确认任务列表里文件名是编辑后的值
- 粘贴两个 URL,确认探测按钮变灰

### CI 预检

- `cd frontend && bun run test`(单元测试通过)
- `cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings`(后端未改,应无影响)
- 前端 lint/typecheck(若项目配置了)

## 5. 风险与回滚

### 风险

- **低**:改动仅前端单文件,4 处均为增量或条件收紧,无逻辑重写
- **低**:不涉及后端、数据模型、磁盘路径,无数据一致性风险
- **中**:URL 变化清空 `fileName` 会丢弃用户手动输入但未探测的内容 —— 这是设计决策(用户已认可),非 bug

### 回滚

单文件 4 处改动,`git checkout HEAD -- frontend/src/components/NewTaskModal.tsx` 即可完全回滚。

## 6. 非目标(YAGNI)

- 不实现"已存在任务"的探测/重命名(用户明确选择仅新建对话框场景)
- 不分离"用户输入"与"探测来源"状态(方案 B,未被选择)
- 不暴露 `FileMetadata` 完整字段(大小/Content-Type)给前端(当前需求仅需文件名)
- 不修改后端 `probe_filename` 返回类型(保持 `Result<String, String>`)
