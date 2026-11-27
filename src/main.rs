// wash 入口：支持 `-c <command>`（非交互，供对拍测试用）与交互式 REPL。
//
// 参数风格与 bash 对齐：
//   * `-c CMD`         —— 执行 CMD 后退出；
//   * `--command CMD`  —— `-c` 的长选项写法；
//   * `-l`             —— 登录 shell（读取 profile 文件）；
//   * `-lc CMD`        —— 上面两个的组合写法；
//   * `-i`             —— 强制交互式。

mod arith;
mod builtins;
mod byteval;
mod cond;
mod error;
mod executor;
mod expand;
mod help_pages;
mod i18n;
mod lexer;
mod macros;
mod parser;
mod plugin;
mod posix;
mod repl;
mod shell;

use std::process::exit;

/// bash 5.3 的算术变量值递归链可到 1022 层，每层递归要消耗若干解析栈帧；
/// 主线程默认 1 MB 栈在约 1000 层就会 SIGSEGV。整个 shell 跑在一个显式配置
/// 大栈的线程里，既能覆盖 bash 的边界，又保留递归超限时的确定性诊断。
const SHELL_STACK_SIZE: usize = 64 * 1024 * 1024;

fn main() {
    // 本地化必须在任何输出之前初始化：`help` 等内建命令的说明文本要按
    // 当前 locale 翻译（`LC_ALL=C` 下是英文，`zh_CN.UTF-8` 下是中文）。
    i18n::init();

    let worker = std::thread::Builder::new()
        .name("wash-shell".to_string())
        .stack_size(SHELL_STACK_SIZE)
        .spawn(run_shell)
        .expect("wash: cannot start shell thread");
    if worker.join().is_err() {
        exit(1);
    }
}

fn run_shell() {
    let args: Vec<String> = std::env::args().collect();
    let mut sh = shell::Shell::new();
    // `$0` 就是 shell 被调用时用的路径，与 bash 一致。
    if let Some(argv0) = args.first() {
        sh.dollar_zero = argv0.clone();
        sh.env.insert("BASH".to_string(), argv0.clone());
    }

    // 解析短选项：`-c`、`-s`、`-l`、`-i` 可以组合（`-lc`、`-sc`、`-cl` ...）。
    let mut want_command = false;
    let mut want_stdin = false;
    let mut login = false;
    let mut force_interactive = false;
    let mut command: Option<String> = None;
    let mut i = 1usize;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.starts_with("--") {
            match a.as_str() {
                "--help" => {
                    println!("wash - a bash-compatible shell with fish-like interactive UX");
                    println!("usage: wash [-l] [-i] [-c <command> | --command <command>]");
                    exit(0);
                }
                "--version" => {
                    println!("wash {}", env!("WASH_VERSION"));
                    exit(0);
                }
                "--command" => {
                    want_command = true;
                    i += 1;
                    break;
                }
                _ => {
                    eprintln!("wash: {}: invalid option", a);
                    exit(2);
                }
            }
        }
        if a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    'c' => want_command = true,
                    's' => want_stdin = true,
                    'l' => login = true,
                    'i' => force_interactive = true,
                    _ => {
                        eprintln!("wash: -{}: invalid option", c);
                        eprintln!("usage: wash [-l] [-i] [-c <command> | --command <command>]");
                        exit(2);
                    }
                }
            }
            i += 1;
            continue;
        }
        break;
    }
    // bash 的参数语义：
    //   `-c CMD NAME ARGS...` -> 执行 CMD，$0=NAME，$1..=ARGS
    //   `SCRIPT ARGS...`      -> 读并执行脚本，$0=SCRIPT，$1..=ARGS
    // 没有额外参数时 $0 保持上面设置的 shell 自身路径。
    let mut script: Option<String> = None;
    let mut command_name: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    if want_command {
        // `-c` 的参数是紧跟其后的那个（`-lc CMD` 也一样）。
        if i >= args.len() {
            eprintln!("wash: -c requires an argument");
            exit(2);
        }
        // 源码文本入口：双写字面哨兵后，decode_raw 才能只把 encode_raw
        // 产生的 ESC+PUA 当字节，避免源码里合法的 PUA 字符被误解码。
        command = Some(crate::byteval::escape_literal(&args[i]));
        if i + 1 < args.len() {
            command_name = Some(args[i + 1].clone());
            positional = args[i + 2..].to_vec();
        }
    } else if i < args.len() {
        script = Some(args[i].clone());
        command_name = Some(args[i].clone());
        positional = args[i + 1..].to_vec();
    }
    if let Some(name) = command_name {
        // `$BASH` stays the shell's own path; only `$0` follows the argv.
        sh.dollar_zero = name;
    }
    sh.positional = positional;

    // `$-` startup flags (fixed for the life of this shell; the mutable
    // single-letter `set` options are read fresh from `sh.options` instead).
    sh.command_string = command.is_some();
    sh.reading_stdin = if command.is_some() {
        want_stdin // `-s` may be combined with `-c`
    } else if script.is_some() {
        false // a script file is neither `-c` nor stdin
    } else {
        true // no `-c` and no script: commands are read from stdin
    };
    sh.dash_interactive = force_interactive
        || (command.is_none() && script.is_none() && posix::isatty(0));
    if sh.dash_interactive {
        // Interactive bash defaults history (`!`) expansion ON (`H` flag).
        sh.options.histexpand = true;
    }

    if login {
        // 登录 shell：读取 profile 文件（与 bash 的顺序一致）。文件不存在或
        // 出错时静默忽略，跟 bash 一样。
        //
        // 读取期间把 wash 自己的语法错误静音：bash 读同一个文件不会报错，
        // wash 的语法覆盖还不全，失败不应该污染命令的输出。
        sh.quiet_errors = true;
        'profile: for f in [
            "/etc/profile",
            "~/.bash_profile",
            "~/.bash_login",
            "~/.profile",
        ] {
            let path = if let Some(rest) = f.strip_prefix("~/") {
                match std::env::var("HOME") {
                    Ok(home) => format!("{}/{}", home, rest),
                    Err(_) => break 'profile,
                }
            } else {
                f.to_string()
            };
            if std::path::Path::new(&path).is_file() {
                sh.quiet_errors = false;
                sh.run_str(&format!(". {}", quote_for_shell(&path)));
                break 'profile;
            }
        }
        sh.quiet_errors = false;
    }

    if let Some(cmd) = command {
        // bash names the input source in parse errors: `-c` for the command
        // string, `exit trap` for an EXIT trap action. Nested run_str calls
        // (eval / source / signal traps) swap the context themselves.
        sh.error_context = Some(shell::ErrorContext::command_string());
        let code = sh.run_str(&cmd);
        // `exit N`（含信号 trap 里的 exit）把最终退出码放在 exit_requested，
        // run_str 返回值只是最后一条命令的状态，不能当进程退出码用。
        // finalize_exit 还会先派发 EXIT trap，trap 内的 `exit N` 可覆盖该码。
        exit(sh.finalize_exit(code));
    }

    if let Some(path) = script {
        // A bare non-option argument is a script file, not a REPL session.
        // bash labels parse errors from it with the script path (no shell
        // name), so the sourced-script error context is the right one.
        let content = match posix::read_file(&path) {
            // read_file 已做 byteval 原始字节编码，不能再 escape_literal，
            // 否则会把 encode_raw 产生的 ESC 标记双写。
            Some(c) => c,
            None => {
                eprintln!("wash: {}: {}", path, posix::errno_message(posix::errno()));
                exit(127);
            }
        };
        let code = sh.run_in_error_context(shell::ErrorContext::sourced(&path), &content);
        // 与 -c 路径一致：显式 exit 码优先，EXIT trap 可覆盖。
        exit(sh.finalize_exit(code));
    }

    // 交互路径：仅当 stdin 是真终端时安装信号捕获，让前台命令期间的
    // Ctrl-C 只中断命令、不杀死 shell；-c / piped 路径保持默认 SIGINT 语义。
    if posix::isatty(0) {
        posix::install_signal_handlers();
    }
    // `-i` 的作用（置交互标志）已在 run_interactive 里体现，这里无需再分流；
    // 两个原本重复的分支合并为一次调用。
    let _ = force_interactive;
    sh.run_interactive();
    // 显式 `exit N` 的码在 sh.exit_requested（EXIT trap 的 run_str 会清空该
    // 标志，repl::run 已在派发 trap 前后保存/恢复）；EOF / Ctrl-D 无显式退出
    // 时，bash 用最后一条命令的状态。对 256 取模对齐进程退出码范围。
    let code = sh.exit_requested.unwrap_or(sh.last_status);
    exit(code.rem_euclid(256));
}

/// 给路径加单引号，便于安全地拼进 `source` 命令里。
fn quote_for_shell(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}
