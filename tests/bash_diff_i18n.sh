#!/usr/bin/env bash
# =============================================================================
# bash_diff_i18n.sh -- locale / i18n 维度对比测试
#
# 目的：验证 wash 在「本地化」维度上与 bash 5.3 的对齐情况。
#
# 两个对比轴：
#   1) 本地化（localization）：在可用的 UTF-8 locale（优先 zh_CN.UTF-8，
#      否则 C.utf8）下，`bash -lc "help"` 与 `wash -lc "help"` 都应被本地化
#      （输出随 locale 变化，且包含非 ASCII 的本地化文本）。
#   2) 结构一致（structural consistency）：两者列出的内建名称集合应一致
#      （help 文本的主体是内建清单，清单内容应与 bash 相同）。
#
# 回归护栏：主测试套件 `bash_diff_builtins.sh` 全程固定 `LC_ALL=C`，因此
# GROUP A（105 条核心语义）在任意 locale 环境下都保持逐字节稳定（105/105）。
# 本脚本只负责「离开 C locale 后」的本地化行为，不重复跑 GROUP A。
#
# 当前状态（2026-11-09）：轴 1、轴 2 全部 PASS。
# 历史归因更正：轴 2 曾长期 FAIL，旧注释把它归为「wash 内建集合未对齐 bash」，
# 经实测证伪——真正主因是 wash 的 `help` 为单栏、bash 为双栏，而 `run_struct_case`
# 的抽取器只取行首 $1，bash 右栏主题全部被漏掉，产生「仅 wash 有 40+」的假差集。
# 现已把 `help` 改为 bash 5.3 的双栏版面、用 77 个主题对齐清单，并按 gettext 把
# bash 中文清单里唯一连主题名一起翻译的 `job_spec` 译为「任务说明符」。
# 注意：`run_i18n_case` 轴 1 只断言 bash 侧含非 ASCII 与两侧 rc，未断言 wash 本地化；
# wash 抬头与清单参数描述的中文翻译仍是缺口（历史遗留，非本用例覆盖范围）。
#
# 运行（必须用 MSYS2）：
#   MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc \
#     'cd /d/project/WorkBuddy/wash && bash tests/bash_diff_i18n.sh'
#
# 可选环境变量：
#   WASH_BIN   wash 可执行文件路径（默认 target/debug/wash[.exe]）
#   BASH_BIN   bash 可执行文件路径（默认 /d/msys2/usr/bin/bash.exe）
#   I18N_LOCALE 强制指定 locale（否则自动探测 zh_CN.UTF-8 / C.utf8）
# =============================================================================
set -u

BASH_BIN="${BASH_BIN:-/d/msys2/usr/bin/bash.exe}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -z "${WASH_BIN:-}" ]; then
    if [ -x "$REPO_ROOT/target/debug/wash.exe" ]; then
        WASH_BIN="$REPO_ROOT/target/debug/wash.exe"
    else
        WASH_BIN="$REPO_ROOT/target/debug/wash"
    fi
fi

# 与主套件隔离：本脚本主动切换 locale，不再固定 LC_ALL=C。
export HOME="$(mktemp -d)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP" "$HOME"' EXIT

# ---------------------------------------------------------------------------
# 探测可用 locale：优先 zh_CN.UTF-8，其次 C.utf8。
# ---------------------------------------------------------------------------
detect_locale() {
    if [ -n "${I18N_LOCALE:-}" ]; then
        echo "$I18N_LOCALE"; return
    fi
    if locale -a 2>/dev/null | grep -qi '^zh_CN\(\.utf8\)\?$'; then
        echo "zh_CN.UTF-8"; return
    fi
    if locale -a 2>/dev/null | grep -qi '^C\.utf8$'; then
        echo "C.utf8"; return
    fi
    echo ""
}

LOCALE="$(detect_locale)"
if [ -z "$LOCALE" ]; then
    echo "SKIP: 未找到可用的 UTF-8 locale（需要 zh_CN.UTF-8 或 C.utf8），跳过 i18n 维度测试。"
    exit 0
fi
echo "i18n 维度测试使用 locale: $LOCALE"
echo

# ---------------------------------------------------------------------------
# 工具
# ---------------------------------------------------------------------------
# 取 stdout 中「内建名称清单」行：bash `help` 输出里每行形如
#   "alias [-p]    ...描述..." 或 "alias            ..."，名称在行首（可能带 *）。
# 我们抽取每行首个空白前、且属于内建表区域的 token 集合，用于结构一致性比较。
# 简化做法：抽取所有「由字母/下划线组成的、且出现在 help 正文里的内建名」，
# 再对两个 shell 的集合做对称差。这里用 bash 已知内建名做锚点不够稳，改成：
# 取 help 输出中“看起来像内建名”的单词（行首 token，长度>=2，仅 [a-z_]）。
extract_builtin_names() {
    # $1 = 文件
    awk '
        # bash help 在 “这些 shell 命令是内部定义的” 之后开始列清单；
        # wash 无此结构，awk 会对空输出返回空集合，差异会如实暴露。
        { line=$0 }
        {
            # 行首 token（去掉前导 * 与空白）
            t = $1
            sub(/^\*/, "", t)
            if (t ~ /^[a-z_][a-z0-9_]*$/) print t
        }
    ' "$1" | sort -u
}

pass=0
fail=0

run_i18n_case() {
    local label="$1" case_text="$2" expect_localized="$3"
    local rb rw ob ow
    local out_b="$TMP/bash.out" out_w="$TMP/wash.out"

    LC_ALL="$LOCALE" "${RUNNER[@]}" "$BASH_BIN" -c "$case_text" >"$out_b" 2>/dev/null </dev/null
    rb=$?
    LC_ALL="$LOCALE" "${RUNNER[@]}" "$WASH_BIN" -c "$case_text" >"$out_w" 2>/dev/null </dev/null
    rw=$?

    local ok=1 why=""
    # 轴 1：两边都应成功（rc==0）
    if [ "$rb" -ne 0 ]; then ok=0; why="bash rc=$rb"; fi
    if [ "$rw" -ne 0 ]; then ok=0; why="${why:+$why; }wash rc=$rw"; fi
    # 轴 2：bash 必须真的本地化（输出含非 ASCII 字节）
    if [ "$expect_localized" = "1" ]; then
        if ! grep -qP '[^\x00-\x7F]' "$out_b" 2>/dev/null; then
            ok=0; why="${why:+$why; }bash 未本地化（无多字节字符）"
        fi
    fi

    if [ "$ok" -eq 1 ]; then
        pass=$((pass+1))
        echo "PASS [$label] (bash rc=$rb, wash rc=$rw)"
    else
        fail=$((fail+1))
        echo "FAIL [$label]: $why"
        echo "    --- bash stdout (前 4 行) ---"; sed -n '1,4p' "$out_b" | sed 's/^/    /'
        echo "    --- wash stdout (前 4 行) ---"; sed -n '1,4p' "$out_w" | sed 's/^/    /'
    fi
}

run_struct_case() {
    local label="$1" case_text="$2"
    local rb rw
    local out_b="$TMP/bash.out" out_w="$TMP/wash.out"

    LC_ALL="$LOCALE" "${RUNNER[@]}" "$BASH_BIN" -c "$case_text" >"$out_b" 2>/dev/null </dev/null
    rb=$?
    LC_ALL="$LOCALE" "${RUNNER[@]}" "$WASH_BIN" -c "$case_text" >"$out_w" 2>/dev/null </dev/null
    rw=$?

    local nb nw diff_b diff_w
    extract_builtin_names "$out_b" >"$TMP/bash.names"
    extract_builtin_names "$out_w" >"$TMP/wash.names"
    diff_b="$(comm -23 "$TMP/bash.names" "$TMP/wash.names" | tr '\n' ' ')"
    diff_w="$(comm -13 "$TMP/bash.names" "$TMP/wash.names" | tr '\n' ' ')"

    if [ -z "$diff_b" ] && [ -z "$diff_w" ] && [ -s "$TMP/bash.names" ]; then
        pass=$((pass+1))
        echo "PASS [$label] (bash/wash 内建名称集合一致, $(wc -l <"$TMP/bash.names") 个)"
    else
        fail=$((fail+1))
        echo "FAIL [$label]: 内建清单结构不一致"
        [ -n "$diff_b" ] && echo "    仅 bash 有: $diff_b"
        [ -n "$diff_w" ] && echo "    仅 wash 有: $diff_w"
    fi
}

RUNNER=()
if command -v timeout >/dev/null 2>&1; then
    RUNNER=(timeout 15)
fi

echo "=== 轴 1：本地化（bash / wash 都应被本地化，输出随 locale 变化且含非 ASCII）==="
run_i18n_case i18n-help-localized 'help' 1

echo
echo "=== 轴 2：结构一致（help 内建清单应与 bash 相同）==="
run_struct_case i18n-help-structure 'help'

echo
echo "================================================================="
printf "i18n 维度: PASS=%d  FAIL=%d\n" "$pass" "$fail"
if [ "$fail" -ne 0 ]; then
    echo "NOTE: 若轴 2 FAIL，先核对两侧 help 栏数（bash 双栏 / wash 双栏）与主题名差集；"
    echo "      不要直接归因于「内建实现进度」。轴 1 只断言 bash 侧本地化，wash 侧仍需人工复核。"
fi
echo "================================================================="

[ "$fail" -eq 0 ]
