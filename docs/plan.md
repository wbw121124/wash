# wash 项目实现计划

> 状态：进行中

## 项目概述

wash (wbw121124's advanced shell) 是一个功能丰富的 shell，支持：
- 变量系统（`%var`、`%env.var`）
- 命令执行与管道
- 控制流（if/elif/else、for、while）
- 函数与 lambda
- 表达式运算（`calc(...)`）
- 命令替换 `$(...)` 和重定向 `@(...)`
- i18n 国际化（中/英文）
- terminfo 彩色终端支持

## 架构设计

### 目录结构

```
wash/
├── CMakeLists.txt
├── docs/
│   ├── index.md          # 语法规范
│   └── plan.md           # 本文件
├── etc/
│   └── washrc            # 系统级 rc 文件
├── po/
│   ├── wash.pot          # 翻译模板
│   ├── zh_CN.po          # 中文翻译
│   └── en_US.po          # 英文翻译
├── share/
│   └── locale/           # 编译后的 .mo 文件
├── src/
│   ├── main.cpp          # 入口点、命令行解析、交互循环
│   ├── types.h           # 类型定义、枚举、AST 节点
│   ├── lexer.h           # 词法分析器接口
│   ├── lexer.cpp         # 词法分析器实现
│   ├── parser.h          # 语法分析器接口
│   ├── parser.cpp        # 语法分析器实现
│   ├── executor.h        # 执行器接口
│   ├── executor.cpp      # 执行器实现
│   ├── color.h           # terminfo 颜色支持
│   └── color.cpp         # 颜色实现
└── ~/.washrc             # 用户级 rc 文件（运行时创建）
```

### 模块划分

| 模块 | 职责 | 文件 |
|------|------|------|
| 类型定义 | Token、AST 节点、值类型（string/int64_t/double） | types.h |
| 词法分析 | 源码 → Token 流 | lexer.h/cpp |
| 语法分析 | Token 流 → AST | parser.h/cpp |
| 执行器 | AST → 执行结果 | executor.h/cpp |
| 颜色支持 | terminfo 颜色查询与输出 | color.h/cpp |
| 主程序 | 入口、交互、参数解析、i18n | main.cpp |

---

## 实现步骤

### 阶段 0：关键 Bug 修复（基础层）

这些 bug 不修，后续功能全部无法正常工作。

| # | Bug | 文件 | 修复方案 |
|---|-----|------|----------|
| 0.1 | `exitScope()` 为空，作用域永不弹出 | executor.cpp | 恢复 `currentScope_ = currentScope_->parent` |
| 0.2 | 函数定义是空操作，body 不执行 | executor.cpp | 存储 AST body，调用时创建子作用域执行 |
| 0.3 | Lambda 定义返回字符串 `"<lambda>"` | executor.cpp | 返回可调用闭包对象 |
| 0.4 | 函数参数拒绝裸标识符 `echo(yes)` | parser.cpp | `parsePrimary` 中非赋值上下文的 IDENTIFIER 作为字符串字面量 |
| 0.5 | 位或解析器检查 AMPERSAND 而非 PIPE | parser.cpp | `parseBitwiseOr()` 改检查 `PIPE` |
| 0.6 | 位异或 `^` 被当作幂运算 | executor.cpp | `^` 改为 XOR |
| 0.7 | ENV_VIEW token 从未生成 | lexer.cpp | `%env` 后无 `.` 时生成 ENV_VIEW |
| 0.8 | LBRACKET token 值为 `]` 应为 `[` | lexer.cpp | 修正值字符串 |
| 0.9 | 三目运算符 `?:` 和逗号运算符未解析 | parser.cpp | 添加 `parseTernary()` 和 `parseComma()` |
| 0.10 | `argc()`/`argv()` 是空壳 | executor.cpp | 连接 main 的 argc/argv 参数 |

### 阶段 1：类型系统 — 新增 int

**目标**：在 Value variant 中新增 `int64_t`，与 `double` 共存。

```cpp
// types.h
using Value = std::variant<std::string, int64_t, double>;
```

**类型推断规则**（在 calc 内）：
- 整数字面量 `42` → `int64_t`
- 浮点字面量 `3.14` → `double`
- `int64_t op int64_t` → `int64_t`（除法 `/` 仍为 `double`）
- `int64_t op double` 或 `double op int64_t` → `double`
- `round()/ceil()/floor()` 返回 `int64_t`
- 位运算 `& | ^ << >>` 操作数强制转 `int64_t`
- 字符串拼接时，int 输出无小数点（`"42"` 而非 `"42.000000"`）

**需改动的文件**：
- `types.h`：Value 定义 + `makeIntValue()` / `makeDoubleValue()` 辅助函数
- `executor.cpp`：所有 `std::holds_alternative<double>` 检查改为同时处理 int64_t
- `parser.cpp`：数字字面量解析区分整数/浮点

### 阶段 2：Parser/Executor 功能补全

#### 2.1 三目与逗号运算符
- parser：在 `parseLogicalOr()` 之后插入 `parseTernary()`，处理 `?:`
- parser：在最外层插入 `parseComma()`，处理 `,`
- executor：已有 `executeTernaryOp()`，补上逗号执行逻辑

#### 2.2 管道表达式
- parser：识别 `$cmd1 | $cmd2` 链，构造 `PipeExprNode` 链
- executor：用 `pipe()` + `fork()` 实现管道串联

#### 2.3 重定向目标
- parser：解析 `@("in.txt", "out.txt", "err.txt")` 的实际参数
- executor：用 `dup2()` + `open()` 实现文件描述符重定向

#### 2.4 行续 `\`
- lexer：`skipWhitespace()` 中检测 `\` + `\n`，合并为单个空白 token

#### 2.5 范围增强
- parser：支持 `a..b..s` 显式步长语法
- executor：验证步长规则（s>0 时 a<=b，s<0 时 a>=b）
- 支持字符串范围 `"1..10,20..40,0"` 和变量范围 `for(%i in %x)`

#### 2.6 字符串插值
- lexer：双引号/反引号中的 `%{var}` 捕获变量名
- executor：在字符串值构建时解析 `%{var}` 并替换为变量值

#### 2.7 相邻字符串自动拼接
- lexer：连续字符串字面量（无运算符分隔）合并为单个 STRING token

### 阶段 3：新内建命令

| 命令 | 签名 | 说明 |
|------|------|------|
| `len()` | `len(string)` → number | 返回字符串长度 |
| `substr()` | `substr(str, start, len)` → string | 子串提取 |
| `find()` | `find(str, sub)` → number | 查找子串位置，-1 表示未找到 |
| `split()` | `split(str, delim)` → range | 按分隔符分割 |
| `join()` | `join(range, delim)` → string | 按分隔符合并 |
| `read()` | `read(prompt?)` → string | 从 stdin 读取一行输入 |
| `source()` | `source(file)` | 加载并执行 .wash 文件 |
| `unset()` | `unset(var)` | 删除变量 |
| `export()` | `export(var)` | 将变量导出为环境变量 |
| `help()` | `help()` | 显示所有内建命令列表 |
| `type()` | `type(name)` | 显示变量/函数类型 |
| `keys()` | `keys()` | 列出所有已定义变量名 |

### 阶段 4：i18n — GNU gettext

**方案**：标准 gettext 工作流，提供中文 (`zh_CN`) 和英文 (`en_US`) 翻译。

**步骤**：
1. 所有用户面字符串用 `_("string")` 标记
2. 生成 `.pot` 模板（`xgettext`）
3. 创建 `po/zh_CN.po` 和 `po/en_US.po` 翻译文件
4. 编译 `.mo` 到 `share/locale/`
5. `main.cpp` 初始化 locale：`setlocale(LC_ALL, "")` + `bindtextdomain()` + `textdomain()`
6. CMakeLists.txt 添加 gettext 编译规则

**安装依赖**：`pacman -S gettext`

### 阶段 5：交互式多行输入

**方案**：在 readline 循环中追踪未闭合的 `{}`、`()`、`[]`，未闭合时显示续行提示符 `> `。

**实现**：
- 新增 `isIncomplete(const std::string& input)` 函数：逐字符计数引号/括号/大括号
- 未闭合时循环追加 readline 输入（续行提示符 `"> "`）
- 闭合后整体提交执行
- 支持 `\` 续行符
- 转义引号内的括号不计入计数

### 阶段 6：terminfo 彩色支持（ncurses）

**安装依赖**：`pacman -S ncurses`

**实现**：
1. CMakeLists.txt 链接 `-lncurses -ltinfo`
2. 新增 `color.h/color.cpp`：
   - 初始化：`setupterm(NULL, STDOUT_FILENO, NULL)`
   - 查询颜色能力：`tigetstr("setaf")` / `tigetstr("setab")` / `tigetstr("sgr0")`
   - 颜色枚举：`COLOR_BLACK` ~ `COLOR_WHITE` + `COLOR_DEFAULT`
   - `color::setfg(color)` / `color::setbg(color)` / `color::reset()`
   - `color::hasColor()` 检测终端是否支持颜色
3. PS1 提示符支持颜色转义
4. 内建命令 `colors()` 显示终端颜色能力

---

## 命令行参数规范

```
wash [选项] [文件名]
```

| 参数 | 说明 |
|------|------|
| 无参数 | 交互模式 |
| `<文件名>` | 执行脚本文件 |
| `-c "命令"` | 执行命令后退出 |
| `-l` | 登录 shell 模式，加载 rc 文件 |

**互斥规则**：`<文件名>` 和 `-c "命令"` 互斥，不能同时使用。

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `%env:washPS1` | `[\u@\h \W]\$` | 提示符 |
| `%env:HISTORY_MAX` | `10000` | 历史记录最大条数 |
| `%env:washLANG` | 系统 locale | 语言设置（zh_CN / en_US） |

## RC 文件

### 系统级 `/etc/washrc`
所有用户共享的配置，登录 shell 时加载。

### 用户级 `~/.washrc`
用户个人配置，登录 shell 时加载。

### 阶段 7：Unicode 字符串支持

**目标**：添加 `u'...'`、`u"..."`、`u`...`` 三种 Unicode 字符串字面量。

**实现**：
1. Lexer 新增 token 类型 `UNICODE_STRING`
2. `scanUnicodeString()` 扫描 `u` 前缀 + 引号字符串
3. 支持 `\uXXXX`（4 位）和 `\UXXXXXXXX`（8 位）Unicode 转义
4. 支持 `\n`、`\t`、`\r` 等常规转义
5. 反引号形式支持多行
6. 执行器中 Unicode 字符串作为普通 `std::string`（UTF-8 编码）存储

### 阶段 8：模块系统（run/include/export/import）

#### 8.1 run("filename", ...args)
- 子环境执行：fork 新进程，继承当前环境
- 执行完毕后子进程环境不污染父进程
- 返回子进程退出码

#### 8.2 include("filename", ...args)
- 当前环境执行：直接解析并执行目标文件
- 函数和变量定义保留在当前作用域
- 支持相对路径和绝对路径

#### 8.3 export(function, variable)
- 将函数/变量标记为可导出
- 存储到模块导出表

#### 8.4 `[%a=%abc, b=xyz] = import("filename", ...args)`
- 语法：方括号内为导入映射，`%a=%abc` 表示将模块中的 `abc` 导入为本地 `%a`
- `b=xyz` 表示将模块中的 `xyz` 导入为本地变量 `b`（无 `%` 前缀）
- 执行目标文件，收集其 export 表，按映射赋值到当前作用域

### 阶段 9：文档与许可证

#### 9.1 更新 docs/index.md
- 添加 Unicode 字符串规范
- 添加 run/include/export/import 规范
- 添加内建命令完整列表
- 添加类型系统说明（string/number/int）

#### 9.2 创建 man 文档
- `wash.1`：用户手册
- 使用 groff 格式
- 覆盖命令行参数、语法概览、内建函数、环境变量

#### 9.3 创建 README.md
- 项目简介、特性列表
- 安装说明（MSYS2 依赖）
- 快速开始、语法示例
- 许可证声明

#### 9.4 添加 MPL 2.0 许可证
- 创建 `LICENSE` 文件
- 源文件头部添加 MPL 2.0 header
- CMakeLists.txt 添加许可证元数据

---

## 执行顺序与依赖

```
阶段 0 (Bug 修复) ✅
    ↓
阶段 1 (int 类型) ← 依赖阶段 0
    ↓
阶段 2 (Parser/Executor 补全) ← 依赖阶段 1
    ↓
阶段 3 (新内建命令) ← 依赖阶段 1
    ↓
阶段 4 (i18n) ← 依赖阶段 3
    ↓
阶段 5 (多行输入) ← 独立
    ↓
阶段 6 (terminfo) ← 独立
    ↓
阶段 7 (Unicode) ← 依赖阶段 2（字符串处理）
    ↓
阶段 8 (模块系统) ← 依赖阶段 2 + 阶段 3
    ↓
阶段 9 (文档与许可证) ← 最后做
```

## 预估工作量

| 阶段 | 预估 commit 数 | 复杂度 |
|------|---------------|--------|
| 0. Bug 修复 | 1 | ★★★☆☆ |
| 1. int 类型 | 1-2 | ★★★☆☆ |
| 2. Parser/Executor | 3-4 | ★★★★☆ |
| 3. 新内建命令 | 2-3 | ★★☆☆☆ |
| 4. i18n | 1-2 | ★★☆☆☆ |
| 5. 多行输入 | 1 | ★★☆☆☆ |
| 6. terminfo | 1-2 | ★★☆☆☆ |
| 7. Unicode | 1-2 | ★★☆☆☆ |
| 8. 模块系统 | 3-4 | ★★★★☆ |
| 9. 文档与许可证 | 2-3 | ★☆☆☆☆ |
| **合计** | **17-24** | |

---

## 进度追踪

### 已完成

- [x] 创建 docs/plan.md
- [x] 修复 CMakeLists.txt 并配置 readline
- [x] 创建基础头文件类型定义 (types.h)
- [x] 实现词法分析器 (lexer.h/cpp)
- [x] 实现语法分析器 (parser.h/cpp)
- [x] 实现执行器 (executor.h/cpp)
- [x] 实现 main.cpp 命令行参数和交互循环
- [x] 实现内建函数和环境变量
- [x] 实现 rc 文件加载
- [x] 实现 history 功能
- [x] 修复 readline 宏冲突 (RETURN/NEWLINE/IN 等)
- [x] 安装 msys64 readline-devel 并切换到 msys64 纯 POSIX 环境
- [x] 首次编译通过并成功运行

### 待完成

- [x] 阶段 0：修复 exitScope() 为空
- [x] 阶段 0：修复函数定义空操作
- [x] 阶段 0：修复 lambda 返回字符串
- [x] 阶段 0：修复函数参数裸标识符解析
- [x] 阶段 0：修复位或解析器 AMPERSAND bug
- [x] 阶段 0：修复 ^ 被当作幂运算
- [x] 阶段 0：修复 ENV_VIEW token 未生成
- [x] 阶段 0：修复 LBRACKET token 值错误
- [x] 阶段 0：实现三目和逗号运算符解析
- [x] 阶段 0：连接 argc/argv 到 main
- [x] 阶段 0：实现范围运算符 .. 和 ..< 解析
- [x] 阶段 1：新增 int64_t 类型
- [x] 阶段 1：修复 % 取模运算符与变量前缀冲突
- [x] 阶段 2：管道表达式
- [x] 阶段 2：重定向目标解析
- [x] 阶段 2：行续 `\`
- [x] 阶段 2：范围增强（步长、逗号分隔）
- [x] 阶段 2：字符串插值
- [x] 阶段 2：相邻字符串自动拼接
- [x] 阶段 3：新内建命令
- [x] 阶段 3：新增内建命令（len/substr/find/split/join/read/source/unset/export/help/type/keys）
- [x] 阶段 4：i18n gettext 支持
- [x] 阶段 5：交互式多行输入
- [x] 阶段 6：terminfo 彩色支持
- [ ] 阶段 5：交互式多行输入
- [ ] 阶段 6：terminfo 彩色支持
- [ ] 阶段 7：Unicode 字符串支持（u'...' u"..." u`...`）
- [ ] 阶段 8.1：run() 子环境执行
- [ ] 阶段 8.2：include() 当前环境执行
- [ ] 阶段 8.3：export() 导出函数/变量
- [ ] 阶段 8.4：import() 模块导入与映射
- [ ] 阶段 9.1：更新 docs/index.md 规范
- [ ] 阶段 9.2：创建 man 文档
- [ ] 阶段 9.3：创建 README.md
- [ ] 阶段 9.4：添加 MPL 2.0 许可证

## 更新日志

| 日期 | 更新内容 |
|------|----------|
| 2026-09-12 | 初始计划创建 |
| 2026-09-12 | 修复 CMakeLists.txt，创建 types.h 类型定义 |
| 2026-09-12 | 实现词法分析器、语法分析器、执行器、主程序 |
| 2026-09-12 | 修复 readline 宏冲突，安装 msys64 readline-devel |
| 2026-09-12 | 切换到 msys64 纯 POSIX 环境，首次编译运行成功 |
| 2026-09-12 | 修订计划：新增 int 类型、i18n、新内建命令、terminfo、多行输入 |
| 2026-09-13 | 阶段 0 完成：修复 10+ 关键 Bug，添加范围运算符、三目/逗号运算符 |
| 2026-09-13 | 追加计划：Unicode 字符串、模块系统(run/include/export/import)、文档、MPL 2.0 |
| 2026-09-13 | 阶段 1 完成：新增 int64_t 类型，修复 % 取模运算符上下文识别 |
