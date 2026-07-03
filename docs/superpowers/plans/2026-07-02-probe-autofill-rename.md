# 新建任务探测后自动填名 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 NewTaskModal 点"探测"后,探测到的文件名自动填入重命名 input 的 value(可编辑),提交时该值生效。

**Architecture:** 方案 A — 最小改动。复用现有 `probedFilename` signal,在 `handleProbe` 成功后同步 `setFileName(name)` 填入重命名 input;URL 变化时清空 `fileName`;批量(>1)时禁用探测按钮;input placeholder 改用 `displayFilename` 保持视觉一致。单文件 4 处改动,后端零改动。

**Tech Stack:** TypeScript + SolidJS + `@solidjs/testing-library` + vitest。组件位于 `frontend/src/components/NewTaskModal.tsx`,测试在 `frontend/src/components/__tests__/NewTaskModal.spec.tsx`(已有文件,加用例)。

## Global Constraints

- 注释/提交信息使用中文,代码标识符使用英文,不使用 emoji(AGENTS.md)
- 提交格式:`<类型>(<范围>): <简要描述>`(中文)
- 提交前:`cargo fmt --all` -> `cargo build --all`(零警告) -> `cargo nextest run --all`(全通过)(AGENTS.md)。本计划仅改前端,后端构建/测试应不受影响,但仍需验证
- 前端 MUST 使用 Bun(AGENTS.md)
- i18n 文案(已存在,无需新增):`newTask.probeFilename`="探测"、`newTask.fileNameLabel`="重命名(可选)"、`newTask.advanced`="高级选项"、URL label 用"下载链接"匹配
- 测试 mock 模式:`vi.mock("../../api/invoke", ...)` mock `api` 对象(参考现有 `NewTaskModal.spec.tsx:5-10`)

## File Structure

- **Modify:** `frontend/src/components/NewTaskModal.tsx` — 4 处改动(handleProbe / URL 变化 effect / 探测按钮 disabled / input placeholder)
- **Test:** `frontend/src/components/__tests__/NewTaskModal.spec.tsx` — 在现有文件内新增"探测后自动填名"测试 describe 块,mock 扩展 `probeFilename`

不新建文件。不改动后端任何 crate。

---

### Task 1: 扩展测试 mock,新增"探测后自动填名"测试用例(TDD - 先写失败测试)

**Files:**
- Test: `frontend/src/components/__tests__/NewTaskModal.spec.tsx:5-10`(扩展 mock)+ 文件末尾新增 describe 块

**Interfaces:**
- Consumes: `api.probeFilename(url: string) => Promise<string>`(已存在于 `frontend/src/api/invoke.ts:99`,Tauri command `probe_filename`)。现有 spec mock 未包含此方法,需补上
- Produces: 无新接口。测试验证 `NewTaskModal` 内部 `handleProbe` 成功后 `setFileName(name)` 的行为

**i18n 文案锚点(测试用 getByLabelText/getByRole 依赖):**
- URL textarea:`screen.getByLabelText(/下载链接/)`(label 文案"下载链接")
- 高级选项展开按钮:`screen.getByRole("button", { name: "高级选项" })`
- 重命名 input:`screen.findByLabelText("重命名(可选)")`(在高级选项展开后出现)
- 探测按钮:`screen.getByRole("button", { name: /探测/ })`(在 `displayFilename()` 有值时显示)

- [ ] **Step 1: 扩展现有 mock,加入 `probeFilename`**

修改 `frontend/src/components/__tests__/NewTaskModal.spec.tsx` 第 5-10 行的 mock 块,从:

```ts
vi.mock("../../api/invoke", () => ({
  api: {
    createTask: vi.fn(),
    pauseTask: vi.fn(),
  },
}));
```

改为:

```ts
vi.mock("../../api/invoke", () => ({
  api: {
    createTask: vi.fn(),
    pauseTask: vi.fn(),
    probeFilename: vi.fn(),
  },
}));
```

在文件顶部 import 行(第 1 行)确认已有 `vi`,若无则补:`import { describe, it, expect, vi, afterEach } from "vitest";`(现有已含,无需改)。

- [ ] **Step 2: 在文件末尾(`}` 闭合 `describe("NewTaskModal", ...)` 之前)新增"探测后自动填名"describe 块**

在第 64 行 `});`(闭合外层 describe)之前插入:

```ts
  describe("探测后自动填名", () => {
    it("探测成功后重命名 input value 填入探测名", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>).mockResolvedValue(
        "model.safetensors",
      );

      render(() => <NewTaskModal onClose={() => {}} />);

      // 输入单个有效 URL 触发 displayFilename 显示探测按钮
      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      // 展开高级选项,重命名 input 出现
      fireEvent.click(screen.getByRole("button", { name: "高级选项" }));
      const fileNameInput = (await screen.findByLabelText(
        "重命名(可选)",
      )) as HTMLInputElement;

      // 点探测前 input value 为空
      expect(fileNameInput.value).toBe("");

      // 点探测
      const probeBtn = screen.getByRole("button", { name: /探测/ });
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("model.safetensors");

      expect(fileNameInput.value).toBe("model.safetensors");
      expect(api.probeFilename).toHaveBeenCalledWith("https://example.com/model");
    });

    it("探测后用户可继续编辑 input", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>).mockResolvedValue(
        "model.safetensors",
      );

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      fireEvent.click(screen.getByRole("button", { name: "高级选项" }));
      const fileNameInput = (await screen.findByLabelText(
        "重命名(可选)",
      )) as HTMLInputElement;

      const probeBtn = screen.getByRole("button", { name: /探测/ });
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("model.safetensors");

      // 用户编辑
      fireEvent.input(fileNameInput, {
        target: { value: "model-renamed.safetensors" },
        currentTarget: { value: "model-renamed.safetensors" },
      });

      expect(fileNameInput.value).toBe("model-renamed.safetensors");
    });

    it("URL 变化后清空已填入的文件名", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>).mockResolvedValue(
        "model.safetensors",
      );

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      fireEvent.click(screen.getByRole("button", { name: "高级选项" }));
      const fileNameInput = (await screen.findByLabelText(
        "重命名(可选)",
      )) as HTMLInputElement;

      const probeBtn = screen.getByRole("button", { name: /探测/ });
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("model.safetensors");
      expect(fileNameInput.value).toBe("model.safetensors");

      // URL 变化
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/other-file" },
        currentTarget: { value: "https://example.com/other-file" },
      });

      expect(fileNameInput.value).toBe("");
    });

    it("重新探测覆盖用户已输入的内容", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>)
        .mockResolvedValueOnce("old.bin")
        .mockResolvedValueOnce("new.bin");

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/file" },
        currentTarget: { value: "https://example.com/file" },
      });

      fireEvent.click(screen.getByRole("button", { name: "高级选项" }));
      const fileNameInput = (await screen.findByLabelText(
        "重命名(可选)",
      )) as HTMLInputElement;

      // 第一次探测填入 old.bin
      const probeBtn = screen.getByRole("button", { name: /探测/ });
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("old.bin");
      expect(fileNameInput.value).toBe("old.bin");

      // 第二次探测覆盖为 new.bin
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("new.bin");
      expect(fileNameInput.value).toBe("new.bin");
    });

    it("批量多 URL 时探测按钮禁用", async () => {
      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: {
          value:
            "https://example.com/file1\nhttps://example.com/file2",
        },
        currentTarget: {
          value:
            "https://example.com/file1\nhttps://example.com/file2",
        },
      });

      // displayFilename 有值时探测按钮渲染;批量时(2 个)应 disabled
      const probeBtn = screen.getByRole("button", { name: /探测/ });
      expect((probeBtn as HTMLButtonElement).disabled).toBe(true);
    });
  });
```

- [ ] **Step 3: 运行测试,确认新增用例失败(实现尚未做)**

Run: `cd frontend && bun run test -- NewTaskModal.spec.tsx`
Expected: 5 个新用例 FAIL —— "探测成功后重命名 input value 填入探测名" 等用例断言 `fileNameInput.value` 为探测名,但当前 `handleProbe` 未 `setFileName`,实际 value 为空。原有的 2 个用例应仍 PASS。

- [ ] **Step 4: 暂不提交(实现尚未做,测试红灯)**

---

### Task 2: 实现 — 4 处改动让测试通过

**Files:**
- Modify: `frontend/src/components/NewTaskModal.tsx`(59-63 行 effect、98-111 行 handleProbe、477-486 行按钮 disabled、578-580 行 placeholder)

**Interfaces:**
- Consumes: 无新接口
- Produces: `handleProbe` 成功后 `setFileName(name)` 的行为变更;URL 变化 effect 清空 `fileName`;探测按钮批量时 disabled;input placeholder 用 `displayFilename`

- [ ] **Step 1: 改动 2 — URL 变化 effect 清空 fileName**

修改 `frontend/src/components/NewTaskModal.tsx` 第 59-63 行,从:

```ts
  // URL 变化时清除探测结果(新 URL 需重新探测)
  createEffect(() => {
    validUrls();
    setProbedFilename(null);
  });
```

改为:

```ts
  // URL 变化时清除探测结果(新 URL 需重新探测),同步清空已填入的文件名
  createEffect(() => {
    validUrls();
    setProbedFilename(null);
    setFileName("");
  });
```

- [ ] **Step 2: 改动 1 — handleProbe 成功后 setFileName(name)**

修改第 98-111 行 `handleProbe`,从:

```ts
  // 探测按钮处理
  const handleProbe = async () => {
    const url = validUrls()[0];
    if (!url) return;
    setProbing(true);
    try {
      const name = await api.probeFilename(url);
      setProbedFilename(name);
    } catch {
      // 探测失败保持本地提取结果
    } finally {
      setProbing(false);
    }
  };
```

改为:

```ts
  // 探测按钮处理:探测成功后把名字填入重命名 input(可编辑,重新探测覆盖)
  const handleProbe = async () => {
    const url = validUrls()[0];
    if (!url) return;
    setProbing(true);
    try {
      const name = await api.probeFilename(url);
      setProbedFilename(name);
      setFileName(name);
    } catch {
      // 探测失败保持本地提取结果
    } finally {
      setProbing(false);
    }
  };
```

- [ ] **Step 3: 改动 3 — 探测按钮批量时 disabled**

修改第 477-486 行探测按钮的 `disabled` 属性,从:

```tsx
            <Button
              variant="ghost"
              size="sm"
              loading={probing()}
              disabled={probing() || validCount() === 0}
              onClick={handleProbe}
            >
```

改为:

```tsx
            <Button
              variant="ghost"
              size="sm"
              loading={probing()}
              disabled={probing() || validCount() !== 1}
              onClick={handleProbe}
            >
```

- [ ] **Step 4: 改动 4 — input placeholder 用 displayFilename**

修改第 578-580 行重命名 input 的 placeholder,从:

```tsx
                    placeholder={
                      suggestedFileName() || tr("newTask.fileNamePlaceholder")
                    }
```

改为:

```tsx
                    placeholder={
                      displayFilename() || tr("newTask.fileNamePlaceholder")
                    }
```

- [ ] **Step 5: 运行测试,确认全部用例通过**

Run: `cd frontend && bun run test -- NewTaskModal.spec.tsx`
Expected: PASS —— 原有 2 个用例 + 新增 5 个用例全部通过。

- [ ] **Step 6: 提交**

```bash
cd D:/Rust/Tachyon
git add frontend/src/components/NewTaskModal.tsx frontend/src/components/__tests__/NewTaskModal.spec.tsx
git commit -m "feat(new-task): 探测后自动填入文件名到重命名输入框"
```

---

### Task 3: 全量验证与 CI 预检

**Files:**
- 无修改,仅运行验证命令

- [ ] **Step 1: 前端全量测试**

Run: `cd frontend && bun run test`
Expected: 全部 PASS,无回归

- [ ] **Step 2: 前端 typecheck(若配置)**

Run: `cd frontend && bun run typecheck 2>/dev/null || bunx tsc --noEmit 2>/dev/null || echo "no typecheck script"`
Expected: 无类型错误。若无 typecheck 脚本则跳过(输出 "no typecheck script")

- [ ] **Step 3: 后端构建与测试(确认未受影响)**

Run: `cd D:/Rust/Tachyon && cargo build --all 2>&1 | tail -5`
Expected: 零警告零错误

Run: `cd D:/Rust/Tachyon && cargo nextest run --all 2>&1 | tail -10`
Expected: 全部 PASS

- [ ] **Step 4: 格式检查**

Run: `cd D:/Rust/Tachyon && cargo fmt --all -- --check`
Expected: 无格式问题

- [ ] **Step 5: 确认工作树干净**

Run: `cd D:/Rust/Tachyon && git status --short`
Expected: 仅显示之前已修改的文件(M 标记),无本任务新增的未提交改动(Task 2 已提交)

- [ ] **Step 6: 无需额外提交(实现已在 Task 2 提交)**

---

## Self-Review

**1. Spec coverage:**

| Spec 需求 | 对应 Task |
|----------|-----------|
| 填入 input value(可编辑) | Task 1 用例 1+2 / Task 2 改动 1 |
| 重新探测覆盖用户输入 | Task 1 用例 4 / Task 2 改动 1(setFileName 每次覆盖) |
| URL 变化清空 input | Task 1 用例 3 / Task 2 改动 2 |
| 批量时禁用探测 | Task 1 用例 5 / Task 2 改动 3 |
| input placeholder 用 displayFilename | Task 2 改动 4 |
| 单元测试 | Task 1 全部 |
| 手动验证 | 未列入计划步骤(人工操作,非自动化),但 Task 3 全量测试覆盖回归 |
| CI 预检 | Task 3 |
| 后端零改动 | Task 3 Step 3 确认 |

无遗漏。

**2. Placeholder scan:** 无 TBD/TODO。所有代码块均为完整可执行代码。测试用例含具体断言值(`model.safetensors` 等)。✅

**3. Type consistency:**
- `api.probeFilename` 签名 `(url: string) => Promise<string>`,mock 用 `mockResolvedValue(string)`,一致 ✅
- `setFileName` 是现有 signal setter(第 27 行 `createSignal("")`),接收 string,改动 1 传 `name: string`,一致 ✅
- `validCount() !== 1` 与现有 `validCount() === 1`(第 561 行重命名 input Show)对齐 ✅
- `displayFilename()` memo 已存在(第 92 行),改动 4 直接引用,无新名称 ✅

**4. 测试 mock 一致性:** Task 1 Step 1 扩展 mock 加入 `probeFilename: vi.fn()`,与现有 `createTask`/`pauseTask` 写法一致;测试内 `await import("../../api/invoke")` 取 mocked 模块(与 vitest hoisted mock 配合)。✅

无问题,计划可执行。
