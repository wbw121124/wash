#!/bin/bash
# ============================================================
# scripts/test-all.sh
# 本地三环境构建 + 测试（commit 前必跑）
#
# 用法（在 MSYS2 bash 中执行）:
#   bash scripts/test-all.sh [msys|mingw64|ucrt64|all]
#   默认 all
#
# 环境:
#   msys    -> MSYSTEM=MSYS     链接 msys-2.0.dll(/usr/bin/g++)
#   mingw64 -> MSYSTEM=MINGW64  /mingw64/bin/g++
#   ucrt64  -> MSYSTEM=UCRT64   /ucrt64/bin/g++
#
# 每个环境使用独立构建目录 build/local/<env>(已在 .gitignore 的 build/ 内),
# 每次从头配置, 保证干净。任一环节失败即该环境记为 FAIL。
# ============================================================

set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

ENV="${1:-all}"
case "$ENV" in
    all|msys|mingw64|ucrt64) ;;
    *) echo "错误: 未知环境 '$ENV'"; echo "用法: bash scripts/test-all.sh [msys|mingw64|ucrt64|all]"; exit 1 ;;
esac

declare -A RESULTS

run_env() {
    local name="$1" msystem="$2" prefix="$3" compiler_opt="$4"
    local build_dir="$ROOT/build/local/$name"
    local log="$ROOT/build/local/$name.log"
    local ok=1

    echo ""
    echo "############################################################"
    echo "# [$name] MSYSTEM=$msystem   prefix=$prefix"
    echo "############################################################"

    export MSYSTEM="$msystem"
    export PATH="$prefix/bin:/usr/bin:/bin"

    rm -rf "$build_dir"
    mkdir -p "$build_dir"
    cd "$build_dir" || { echo "[$name] 无法创建构建目录"; RESULTS[$name]=FAIL; return 1; }

    echo "--- [$name] cmake configure ---"
    cmake -G "Unix Makefiles" -DCMAKE_BUILD_TYPE=Release $compiler_opt "$ROOT" >"$log" 2>&1 \
        || { echo "[$name] cmake configure 失败(见 $log)"; tail -20 "$log"; RESULTS[$name]=FAIL; return 1; }

    echo "--- [$name] make -j$(nproc) ---"
    make -j"$(nproc)" >>"$log" 2>&1 \
        || { echo "[$name] 编译失败(见 $log)"; tail -30 "$log"; RESULTS[$name]=FAIL; return 1; }

    echo "--- [$name] wash_test ---"
    ./wash_test || { echo "[$name] wash_test 失败"; RESULTS[$name]=FAIL; return 1; }

    echo "--- [$name] wash_test3 ---"
    ./wash_test3 || { echo "[$name] wash_test3 失败"; RESULTS[$name]=FAIL; return 1; }

    echo "--- [$name] i18n 冒烟(washLANG zh_CN help(echo)) ---"
    local i18n_out
    i18n_out="$(LC_ALL=zh_CN.UTF-8 ./wash.exe -c 'help("echo")' 2>&1)"
    if printf '%s' "$i18n_out" | grep -q "输出参数到 stdout"; then
        echo "[$name] i18n OK"
    else
        echo "[$name] i18n 失败, 输出:"
        printf '%s\n' "$i18n_out"
        RESULTS[$name]=FAIL
        return 1
    fi

    RESULTS[$name]=PASS
    echo "[$name] 全部通过"
}

if [ "$ENV" = "all" ] || [ "$ENV" = "msys" ]; then
    run_env msys MSYS /usr ""
fi
if [ "$ENV" = "all" ] || [ "$ENV" = "mingw64" ]; then
    run_env mingw64 MINGW64 /mingw64 "-DCMAKE_CXX_COMPILER=/mingw64/bin/g++"
fi
if [ "$ENV" = "all" ] || [ "$ENV" = "ucrt64" ]; then
    run_env ucrt64 UCRT64 /ucrt64 "-DCMAKE_CXX_COMPILER=/ucrt64/bin/g++"
fi

echo ""
echo "=========================== 汇总 ==========================="
local_overall=1
for key in msys mingw64 ucrt64; do
    if [ -n "${RESULTS[$key]:-}" ]; then
        echo "  $key: ${RESULTS[$key]}"
        [ "${RESULTS[$key]}" = "PASS" ] || local_overall=0
    fi
done
echo "============================================================"

[ "$local_overall" = 1 ] && echo "全部环境测试通过" || { echo "存在失败环境"; exit 1; }