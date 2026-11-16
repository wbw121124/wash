# 版本串

所有用户可见的版本行都由 `tools/wash_version.sh` 生成，唯一真源是 `Cargo.toml` 的
`version` 加上构建时的 git 元数据。规则只有这一份，代码与测试不得有第二份副本：

- HEAD 恰好有 tag：该 tag（去掉 `v`）**必须**等于 `Cargo.toml` 版本，输出 `<version>`；
- HEAD 没有 tag：输出 `<version>-<HEAD hash 前 7 位>`；
- 最近可达 tag 比 `Cargo.toml` 更新（"打了 tag 却忘了改 Cargo.toml"）、tag 与清单不一致、
  或 git 元数据缺失：构建与闸门**直接报错**，绝不静默回退到旧版本。

`build.rs` 调该脚本并把结果注入 `WASH_VERSION`；`--version`、`$BASH_VERSION`、
`help` 抬头、`help -m` 的 IMPLEMENTATION 段、PS1 `\v` 全部使用 `env!("WASH_VERSION")`，
源码与测试中不含任何硬编码版本号、不用 `CARGO_PKG_VERSION`。
两个 `.po` 头由 `tools/wash_version.sh --sync-po` 生成，`--check-po` 校验新鲜度。
`tests/check_version.sh` 做静态 + 运行时严格校验；`tests/bash_diff_help.sh` 把 oracle
的版本行替换为生成值后做完整逐字节对拍（含可逆性），不跳过任何一行。

WSL 中无 `.git` 的构建树须设 `WASH_GIT_DIR` 指向主仓，否则硬失败。

**每次 commit 都会改变 HEAD hash → 版本串随之改变**：提交后必须重建 debug + release，
再跑 `tests/check_version.sh`。po 头是 base 版本，不受 hash 影响。

发布历史与兼容边界见 `CHANGELOG.md`。
