# wash (wbw121124's advanced shell)

wash 是一个功能丰富的现代 shell，支持变量系统、控制流、函数、表达式运算、管道、重定向、国际化和模块系统。

## 特性

- **变量系统**：`%var` 变量、`%env.var` 环境变量、字符串插值 `%{var}`
- **命令执行**：`$cmd` 命令、`$(...)` 命令替换、管道 `$cmd1 | $cmd2`
- **控制流**：if/elif/else、for、while、break、continue
- **函数**：命名参数、lambda 表达式、闭包
- **表达式**：`calc(...)` 数学表达式、三目运算符 `?:`
- **字符串**：Unicode 字符串 `u'...'`、`u"..."`、`u`...``，支持 `\uXXXX` 和 `\UXXXXXXXX` 转义
- **重定向**：`@("in.txt", "out.txt", "err.txt")` 文件描述符重定向
- **模块系统**：`run()`、`include()`、`export()`、`import()` 模块化编程
- **国际化**：GNU gettext 支持，提供中文和英文翻译
- **彩色终端**：基于 terminfo 的颜色输出
- **多行输入**：自动检测未闭合的括号/引号，支持多行输入

## 安装

### 预编译包

从 [GitHub Releases](https://github.com/wbw121124/wash/releases) 下载预编译包：

| 平台 | 架构 | 文件 |
|------|------|------|
| Windows | x86_64 | `wash-*-mingw-x86_64.zip` |
| Windows | i686 | `wash-*-mingw-i686.zip` |
| Windows | UCRT x86_64 | `wash-*-ucrt-x86_64.zip` |
| Linux | x86_64 | `wash-*-linux-x86_64.tar.gz` |
| Linux | aarch64 | `wash-*-linux-aarch64.tar.gz` |
| Linux | armv7l | `wash-*-linux-armv7l.tar.gz` |

### 从源码编译

#### 依赖

- CMake 3.16+
- GCC 17+ (或 Clang 12+)
- GNU Readline
- GNU gettext
- ncurses

#### MSYS2 (MinGW)

```bash
# 安装依赖
pacman -S mingw-w64-x86_64-gcc mingw-w64-x86_64-cmake \
          msys/readline-devel msys/gettext-devel msys/ncurses-devel

# 编译
cd wash
mkdir build && cd build
cmake -G 'Unix Makefiles' ..
make -j4

# 安装到 /usr/local
sudo cmake --install . --prefix /usr/local
```

#### MSYS2 (UCRT64)

```bash
# 安装依赖
pacman -S mingw-w64-ucrt-x86_64-gcc mingw-w64-ucrt-x86_64-cmake \
          msys/readline-devel msys/gettext-devel msys/ncurses-devel

# 编译
cd wash
mkdir build && cd build
cmake -G 'Unix Makefiles' ..
make -j4

# 安装
sudo cmake --install . --prefix /ucrt64
```

#### Linux (Debian/Ubuntu)

```bash
# 安装依赖
sudo apt install build-essential cmake libreadline-dev gettext libncurses-dev

# 编译
cd wash
mkdir build && cd build
cmake -G 'Unix Makefiles' ..
make -j4

# 安装
sudo cmake --install . --prefix /usr/local
```

#### Linux (Fedora/RHEL)

```bash
# 安装依赖
sudo dnf install gcc-c++ cmake readline-devel gettext-devel ncurses-devel

# 编译
cd wash
mkdir build && cd build
cmake -G 'Unix Makefiles' ..
make -j4

# 安装
sudo cmake --install . --prefix /usr/local
```

#### macOS

```bash
# 安装依赖
brew install cmake readline gettext ncurses

# 编译
cd wash
mkdir build && cd build
cmake -G 'Unix Makefiles' ..
make -j4

# 安装
sudo cmake --install . --prefix /usr/local
```

### 使用 Docker

```bash
docker build -t wash .
docker run -it wash
```

## 使用

### 交互模式

```bash
./wash
# 或
wash
```

### 执行脚本

```bash
./wash script.wash
# 或
wash script.wash
```

### 执行命令

```bash
./wash -c "calc(1 + 2)"
```

## 语法示例

```bash
# 变量
%name = "wash"
echo("Hello, %{name}!")

# 数学表达式
%result = calc(2 + 3 * 4)

# 控制流
for(%i in 1..10) {
    echo(%i)
}

# 函数
fadd(a, b) {
    return(calc(%a + %b))
}
%sum = fadd(3, 4)

# 管道
$ls | $grep("wash")

# 重定向
echo("hello") | @("out.txt", "err.txt")

# Unicode 字符串
%greeting = u"你好世界"
%emoji = u"\u{1F600}"

# 模块系统
include("utils.wash")
%result = run("script.wash", "arg1", "arg2")
```

## 内建函数

| 函数 | 说明 |
|------|------|
| `echo(...)` | 输出到 stdout |
| `stderr(...)` | 输出到 stderr |
| `panic(...)` | 输出到 stderr 并退出 |
| `round(n)` | 四舍五入 |
| `ceil(n)` | 向上取整 |
| `floor(n)` | 向下取整 |
| `length(s)` | 字符串长度 |
| `upper(s)` | 转大写 |
| `lower(s)` | 转小写 |
| `trim(s)` | 去除首尾空白 |
| `substr(s,i,n)` | 子串提取 |
| `find(s,sub)` | 查找子串位置 |
| `split(s,delim)` | 分割字符串 |
| `join(range,d)` | 合并为字符串 |
| `read(prompt?)` | 从 stdin 读取一行 |
| `source(file)` | 加载执行 .wash 文件 |
| `unset(var)` | 删除变量 |
| `export(var)` | 导出为环境变量 |
| `typeof(v)` | 返回类型名 |
| `help()` | 显示帮助 |
| `argc()` | 脚本参数个数 |
| `argv(i)` | 获取脚本参数 |
| `return(v)` | 从函数返回值 |
| `break()` | 跳出循环 |
| `continue()` | 继续下一次循环 |
| `colors()` | 显示终端颜色能力 |
| `run(file,...)` | 子环境执行 |
| `include(file)` | 当前环境执行 |
| `export(name)` | 导出函数/变量 |
| `import(file,map)` | 模块导入 |

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `%env:washPS1` | `[\u@\h \W]\$` | 提示符 |
| `%env:washHISTORY_MAX` | `10000` | 历史记录最大条数 |
| `%env:washLANG` | 系统 locale | 语言设置（zh_CN / en_US） |

## 卸载

```bash
sudo cmake --install build --component Unspecified
# 或手动删除
sudo rm /usr/local/bin/wash
```

## 许可证

Mozilla Public License 2.0 (MPL-2.0)
