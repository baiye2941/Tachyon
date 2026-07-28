/// <reference types="node" />
import { describe, it, expect, beforeAll, afterAll } from "vitest";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import {
  HEX_RE,
  RGB_RE,
  isAllowlisted,
  isCommentLine,
  scanColors,
} from "../../../scripts/lint-color-hardcode";

// 本文件自身也在 lint:colors 扫描范围内,测试输入中的 hex/rgba 一律拼接构造,
// 避免源码里的颜色字面量被守护脚本误判为违规。
const hex = (body: string): string => `#${body}`;
const rgba = (inner: string): string => ["rgba(", inner, ")"].join("");

const matchHex = (text: string): string[] => text.match(HEX_RE) ?? [];

describe("lint-color-hardcode 守护脚本契约", () => {
  describe("HEX_RE(3~8 位 + 边界)", () => {
    it("匹配 3 位 hex(重构前仅 6 位,3 位会逃逸)", () => {
      expect(matchHex(`color: '${hex("fff")}'`)).toEqual([hex("fff")]);
    });

    it("匹配 6 位与 8 位(含 alpha)hex", () => {
      expect(matchHex(hex("A855F7"))).toEqual([hex("A855F7")]);
      expect(matchHex(hex("22c55eff"))).toEqual([hex("22c55eff")]);
    });

    it("不匹配 2 位与 9 位以上连续 hex(\\b 边界)", () => {
      expect(matchHex(hex("ff"))).toEqual([]);
      expect(matchHex(`#${"f".repeat(9)}`)).toEqual([]);
    });

    it("检出 var() fallback 中的硬编码 hex", () => {
      const line = `color: var(--color-warning, ${hex("f59e0b")})`;
      expect(matchHex(line)).toEqual([hex("f59e0b")]);
    });
  });

  describe("RGB_RE(相邻行不漏检)", () => {
    it("连续两次 test 结果一致(不受 /g lastIndex 状态影响)", () => {
      const line = `ctx.fillStyle = "${rgba("0,0,0,0.5")}";`;
      expect(RGB_RE.test(line)).toBe(true);
      // /g 状态下第二次 test 会从 lastIndex 起跳,相邻行因此被漏检
      expect(RGB_RE.test(line)).toBe(true);
    });
  });

  describe("isCommentLine(注释行识别)", () => {
    it("识别 // 行注释与 * 块注释续行(既有行为)", () => {
      expect(isCommentLine("// 行注释")).toBe(true);
      expect(isCommentLine("   * 块注释续行")).toBe(true);
    });

    it("识别 /* 块注释起始与 {/* JSX 注释(新增)", () => {
      expect(isCommentLine("/* 块注释 */")).toBe(true);
      expect(isCommentLine("{/* JSX 注释 */}")).toBe(true);
      expect(isCommentLine("    {/* 缩进 JSX 注释 */}")).toBe(true);
    });

    it("不误伤真实代码行", () => {
      expect(isCommentLine(`const c = "${hex("fff")}";`)).toBe(false);
    });
  });

  describe("isAllowlisted(白名单)", () => {
    it("豁免 styles/ 目录下所有 CSS(token 与样式实体所在)", () => {
      expect(isAllowlisted("styles/tokens.css")).toBe(true);
      expect(isAllowlisted("styles/components/button.css")).toBe(true);
      expect(isAllowlisted("styles/utilities.css")).toBe(true);
    });

    it("保留既有文件级豁免", () => {
      expect(isAllowlisted("index.css")).toBe(true);
      expect(isAllowlisted("utils/format.ts")).toBe(true);
      expect(isAllowlisted("utils/resolveToken.ts")).toBe(true);
      expect(isAllowlisted("components/__tests__/Accessibility.spec.tsx")).toBe(
        true,
      );
    });

    it("豁免 Canvas 组件与颜色断言/算法自验证测试", () => {
      // Canvas fillStyle/渐变无法引用 var(),ChunkMatrix 颜色已走 resolveToken 机制
      expect(isAllowlisted("components/ChunkMatrix.tsx")).toBe(true);
      expect(isAllowlisted("components/__tests__/ChunkMatrix.spec.tsx")).toBe(
        true,
      );
      expect(
        isAllowlisted("test-utils/__tests__/compositeContrast.spec.ts"),
      ).toBe(true);
    });

    it("普通组件与 styles/ 下非 CSS 文件不豁免", () => {
      expect(isAllowlisted("components/ModelLibrary.tsx")).toBe(false);
      expect(isAllowlisted("components/settings/tabs/MagnetTab.tsx")).toBe(
        false,
      );
      expect(isAllowlisted("styles/helper.ts")).toBe(false);
      expect(isAllowlisted("styles-fake/x.css")).toBe(false);
    });
  });

  describe("scanColors(夹具集成)", () => {
    let fixtureDir = "";
    beforeAll(() => {
      fixtureDir = mkdtempSync(join(tmpdir(), "lint-colors-"));
    });
    afterAll(() => {
      rmSync(fixtureDir, { recursive: true, force: true });
    });

    const writeFixture = (rel: string, content: string): void => {
      const full = join(fixtureDir, rel);
      mkdirSync(dirname(full), { recursive: true });
      writeFileSync(full, content);
    };

    it("检出 tsx 中的 3 位 hex(回归:扩位前会逃逸)", () => {
      writeFixture("Bad.tsx", `const c = "${hex("fff")}";\n`);
      const hits = scanColors(fixtureDir).filter((v) => v.file === "Bad.tsx");
      expect(hits).toHaveLength(1);
      expect(hits[0]!.line).toBe(1);
      expect(hits[0]!.value).toBe(hex("fff"));
    });

    it("JSX 注释行里的 HTML 实体不误报为 5 位 hex", () => {
      const entity = `&${"#"}10003;`;
      writeFixture("JsxComment.tsx", `{/* 对勾 ${entity} */}\n`);
      const hits = scanColors(fixtureDir).filter(
        (v) => v.file === "JsxComment.tsx",
      );
      expect(hits).toEqual([]);
    });

    it("styles/ 下 CSS 与各类注释行不产生违规", () => {
      writeFixture("styles/vars.css", `:root { --c: ${hex("ffffff")}; }\n`);
      writeFixture(
        "Commented.tsx",
        `// ${hex("abcdef")}\n/* ${hex("abcdef")} */\n * ${hex("abcdef")}\n`,
      );
      const violations = scanColors(fixtureDir);
      expect(violations.filter((v) => v.file === "styles/vars.css")).toEqual(
        [],
      );
      expect(violations.filter((v) => v.file === "Commented.tsx")).toEqual([]);
    });

    it("相邻两行 rgba 均被检出(回归:/g lastIndex 曾隔行漏检)", () => {
      writeFixture(
        "Rgba.tsx",
        `ctx.fillStyle = "${rgba("255,255,255,0")}";\nctx.fillStyle = "${rgba("255,255,255,0.45")}";\n`,
      );
      const hits = scanColors(fixtureDir).filter((v) => v.file === "Rgba.tsx");
      expect(hits).toHaveLength(2);
    });
  });

  describe("真实违规修复(契约 b)", () => {
    const readSrc = (rel: string): string =>
      readFileSync(resolve(process.cwd(), rel), "utf-8");

    it("ModelLibrary 下载完成徽章使用 --color-accent-foreground token", () => {
      const content = readSrc("src/components/ModelLibrary.tsx");
      expect(content).not.toContain(`'${hex("fff")}'`);
      expect(content).toContain("'var(--color-accent-foreground)'");
    });

    it("MagnetTab 移除全部 var() 硬编码 fallback 与裸 hex", () => {
      const content = readSrc("src/components/settings/tabs/MagnetTab.tsx");
      // var(--x, #fallback) 形式一律禁止
      expect(content.match(/var\(--[\w-]+,\s*#[0-9A-Fa-f]/)).toBeNull();
      expect(content.match(HEX_RE)).toBeNull();
    });

    it("MagnetTab 悬空 --color-border 引用改为 --color-border-default", () => {
      const content = readSrc("src/components/settings/tabs/MagnetTab.tsx");
      expect(content).not.toContain("var(--color-border,");
      expect(content).not.toContain("var(--color-border)");
      expect(content).toContain("var(--color-border-default)");
    });

    it("ConnectionTab 的 --color-warning 不再带硬编码 fallback", () => {
      const content = readSrc("src/components/settings/tabs/ConnectionTab.tsx");
      expect(content).toContain("var(--color-warning)");
      expect(content).not.toContain("var(--color-warning,");
      expect(content).not.toContain(hex("f59e0b"));
    });

    it("被引用的 token 在 tokens.css 中均有定义(防悬空)", () => {
      const tokens = readSrc("src/styles/tokens.css");
      for (const name of [
        "--color-accent-foreground",
        "--color-border-default",
        "--color-bg-secondary",
        "--color-text-secondary",
        "--color-success",
        "--color-warning",
      ]) {
        expect(tokens).toContain(`${name}:`);
      }
    });
  });

  describe("仓库级回归门禁", () => {
    it("src 全量扫描零违规(与 bun run lint:colors 等价)", () => {
      const violations = scanColors(resolve(process.cwd(), "src"));
      expect(violations).toEqual([]);
    });
  });

  describe("CI 接入(契约 c)", () => {
    it("frontend job 在 ESLint 后、前端单元测试前执行 lint:colors", () => {
      const ci = readFileSync(
        resolve(process.cwd(), "../.github/workflows/ci.yml"),
        "utf-8",
      );
      const eslintIdx = ci.indexOf("name: ESLint 检查");
      const colorIdx = ci.indexOf("bun run lint:colors");
      const unitTestIdx = ci.indexOf("name: 前端单元测试");
      expect(eslintIdx).toBeGreaterThanOrEqual(0);
      expect(colorIdx).toBeGreaterThanOrEqual(0);
      expect(unitTestIdx).toBeGreaterThanOrEqual(0);
      expect(colorIdx).toBeGreaterThan(eslintIdx);
      expect(colorIdx).toBeLessThan(unitTestIdx);
      // 步骤需带 working-directory: frontend(与相邻步骤风格一致)
      const stepBlock = ci.slice(Math.max(0, colorIdx - 200), colorIdx);
      expect(stepBlock).toContain("working-directory: frontend");
    });
  });
});
