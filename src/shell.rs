// Shell state + REPL driver.
//
// Holds the environment, positional parameters, last exit status, and the
// current directory. `run_str` is the single command-execution entry point
// shared by `-c` and the interactive REPL (in `repl.rs`). The interactive
// fish-style line editor lives entirely in `repl.rs`; this module only owns
// the persistent shell state and dispatches to the executor.

use std::collections::{HashMap, HashSet};

use crate::error::{DiagnosticFamily, WashError};
use crate::executor;
use crate::lexer;
use crate::parser;
use crate::plugin;
use crate::posix;

/// The absolute path of the running shell binary.
///
/// bash resolves its own name through `$PATH` and uses the result for `$_`
/// and `$BASH`.  wash uses the real path of its own executable, which is the
/// same idea expressed in terms of the Rust runtime.
fn shell_self_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "wash".to_string())
}

/// Shell option flags (mirrors bash's `set -o` table). Defaults are set to
/// match a freshly-started NON-INTERACTIVE bash (`bash -c`).
#[derive(Clone, Debug)]
pub struct SetOptions {
    pub allexport: bool,
    pub braceexpand: bool,
    pub emacs: bool,
    pub errexit: bool,
    pub errtrace: bool,
    pub functrace: bool,
    pub hashall: bool,
    pub histexpand: bool,
    pub history: bool,
    pub igncr: bool,
    pub ignoreeof: bool,
    pub interactive_comments: bool,
    pub keyword: bool,
    pub monitor: bool,
    pub noclobber: bool,
    pub noexec: bool,
    pub noglob: bool,
    pub nolog: bool,
    pub notify: bool,
    pub nounset: bool,
    pub onecmd: bool,
    pub physical: bool,
    pub pipefail: bool,
    pub posix: bool,
    pub privileged: bool,
    pub verbose: bool,
    pub vi: bool,
    pub xtrace: bool,
}

impl Default for SetOptions {
    fn default() -> Self {
        SetOptions {
            allexport: false,
            braceexpand: true,
            emacs: false,
            errexit: false,
            errtrace: false,
            functrace: false,
            hashall: true,
            histexpand: false,
            history: false,
            igncr: false,
            ignoreeof: false,
            interactive_comments: true,
            keyword: false,
            monitor: false,
            noclobber: false,
            noexec: false,
            noglob: false,
            nolog: false,
            notify: false,
            nounset: false,
            onecmd: false,
            physical: false,
            pipefail: false,
            posix: false,
            privileged: false,
            verbose: false,
            vi: false,
            xtrace: false,
        }
    }
}

/// The `set -o` option names in bash 5.3's display order.
pub const SET_O_ORDER: &[&str] = &[
    "allexport",
    "braceexpand",
    "emacs",
    "errexit",
    "errtrace",
    "functrace",
    "hashall",
    "histexpand",
    "history",
    // `igncr` exists only in the Cygwin fork of bash (CRLF handling on
    // Windows). Real Linux bash 5.3.0 does not list it in `set -o` and
    // rejects `set -o igncr` with "invalid option name" (rc 2) -- verified
    // against both oracles.
    #[cfg(target_os = "cygwin")]
    "igncr",
    "ignoreeof",
    "interactive-comments",
    "keyword",
    "monitor",
    "noclobber",
    "noexec",
    "noglob",
    "nolog",
    "notify",
    "nounset",
    "onecmd",
    "physical",
    "pipefail",
    "posix",
    "privileged",
    "verbose",
    "vi",
    "xtrace",
];

impl SetOptions {
    /// Look up an option by its `set -o` name.
    pub fn get(&self, name: &str) -> Option<bool> {
        Some(match name {
            "allexport" => self.allexport,
            "braceexpand" => self.braceexpand,
            "emacs" => self.emacs,
            "errexit" => self.errexit,
            "errtrace" => self.errtrace,
            "functrace" => self.functrace,
            "hashall" => self.hashall,
            "histexpand" => self.histexpand,
            "history" => self.history,
            "igncr" if cfg!(target_os = "cygwin") => self.igncr,
            "ignoreeof" => self.ignoreeof,
            "interactive-comments" => self.interactive_comments,
            "keyword" => self.keyword,
            "monitor" => self.monitor,
            "noclobber" => self.noclobber,
            "noexec" => self.noexec,
            "noglob" => self.noglob,
            "nolog" => self.nolog,
            "notify" => self.notify,
            "nounset" => self.nounset,
            "onecmd" => self.onecmd,
            "physical" => self.physical,
            "pipefail" => self.pipefail,
            "posix" => self.posix,
            "privileged" => self.privileged,
            "verbose" => self.verbose,
            "vi" => self.vi,
            "xtrace" => self.xtrace,
            _ => return None,
        })
    }

    /// Set an option by its `set -o` name. Returns false for unknown names.
    pub fn set(&mut self, name: &str, value: bool) -> bool {
        match name {
            "allexport" => self.allexport = value,
            "braceexpand" => self.braceexpand = value,
            "emacs" => self.emacs = value,
            "errexit" => self.errexit = value,
            "errtrace" => self.errtrace = value,
            "functrace" => self.functrace = value,
            "hashall" => self.hashall = value,
            "histexpand" => self.histexpand = value,
            "history" => self.history = value,
            "igncr" if cfg!(target_os = "cygwin") => self.igncr = value,
            "ignoreeof" => self.ignoreeof = value,
            "interactive-comments" => self.interactive_comments = value,
            "keyword" => self.keyword = value,
            "monitor" => self.monitor = value,
            "noclobber" => self.noclobber = value,
            "noexec" => self.noexec = value,
            "noglob" => self.noglob = value,
            "nolog" => self.nolog = value,
            "notify" => self.notify = value,
            "nounset" => self.nounset = value,
            "onecmd" => self.onecmd = value,
            "physical" => self.physical = value,
            "pipefail" => self.pipefail = value,
            "posix" => self.posix = value,
            "privileged" => self.privileged = value,
            "verbose" => self.verbose = value,
            "vi" => self.vi = value,
            "xtrace" => self.xtrace = value,
            _ => return false,
        }
        true
    }
}

/// The variable attributes a subshell inherits from its parent: which
/// variables are exported, which have no value at all, and which carry the
/// readonly / integer attributes.
///
/// `AttrRefs` is the zero-cost borrowed view used while EXPANDING a command:
/// building an owned `VarAttrs` clones four `HashSet`s for every simple
/// command (hundreds of thousands of node clones inside a tight loop), while
/// expansion only ever needs to look names up.  `VarAttrs` remains the owned
/// form for the places that genuinely need a copy.
pub struct AttrRefs<'a> {
    pub exported: &'a HashSet<String>,
    pub null_exported: &'a HashSet<String>,
    pub readonly: &'a HashSet<String>,
    pub integer_vars: &'a HashSet<String>,
    /// `enable -n` disabled builtins.  A subshell (pipeline stage / command
    /// substitution / forked compound command) inherits this table from its
    /// parent, so `enable -a | grep ...` sees the same disabled entries that
    /// `enable -a` does in the parent shell (verified against MSYS2 bash 5.3).
    pub disabled_builtins: &'a HashSet<String>,
}

impl Shell {
    /// The borrowed view of this shell's variable attributes (no clone).
    pub fn attr_refs(&self) -> AttrRefs<'_> {
        AttrRefs {
            exported: &self.exported,
            null_exported: &self.null_exported,
            readonly: &self.readonly,
            integer_vars: &self.integer_vars,
            disabled_builtins: &self.disabled_builtins,
        }
    }

    /// Copy the variable attributes referred to by `attrs` into this shell
    /// (the owned counterpart of [`Shell::attr_refs`]).
    pub fn apply_attr_refs(&mut self, attrs: &AttrRefs<'_>) {
        self.exported = attrs.exported.clone();
        self.null_exported = attrs.null_exported.clone();
        self.readonly = attrs.readonly.clone();
        self.integer_vars = attrs.integer_vars.clone();
        self.disabled_builtins = attrs.disabled_builtins.clone();
    }
}

/// The `shopt` options wash understands, in bash's display order
/// (alphabetical).  These are the shell options that are NOT reachable
/// through `set -o`.
pub const SHOPT_ORDER: &[&str] = &["expand_aliases", "sourcepath"];

impl Shell {
    /// Look up a `shopt` option by its name.
    pub fn shopt_get(&self, name: &str) -> Option<bool> {
        Some(match name {
            "expand_aliases" => self.expand_aliases,
            "sourcepath" => self.sourcepath,
            _ => return None,
        })
    }

    /// Set a `shopt` option by its name.  Returns false for unknown names.
    pub fn shopt_set(&mut self, name: &str, value: bool) -> bool {
        match name {
            "expand_aliases" => self.expand_aliases = value,
            "sourcepath" => self.sourcepath = value,
            _ => return false,
        }
        true
    }
}

/// 一条后台作业的真实记录，供 `jobs` / `fg` / `bg` / `disown` 使用。
///
/// `pid` 是 `execute_background` 真正 fork 出来的子进程 pid；`command` 是
/// 该作业流水线渲染出的命令文本（不含结尾的 `&`）。所有条目都来自真实启动
/// 的后台作业，不预置、不伪造。`done` / `code` 记录作业是否已被探测为结束。
#[derive(Debug, Clone)]
pub struct JobInfo {
    pub pid: i32,
    pub command: String,
    /// 已被非阻塞探测判定为结束（进程已消失），不再计入 Running。
    pub done: bool,
}

/// bash names the input source in parse-error prefixes: `-c` for a command
/// string, `eval` for the eval builtin, `exit trap` / `trap` for trap
/// actions, and the script's own path for `source`. Sourced scripts omit
/// the shell-name prefix entirely; every other source prints it.
#[derive(Clone, Debug)]
pub struct ErrorContext {
    pub label: String,
    pub with_shell_name: bool,
}

impl ErrorContext {
    pub fn command_string() -> Self {
        ErrorContext {
            label: "-c".to_string(),
            with_shell_name: true,
        }
    }
    pub fn eval() -> Self {
        ErrorContext {
            label: "eval".to_string(),
            with_shell_name: true,
        }
    }
    pub fn exit_trap() -> Self {
        ErrorContext {
            label: "exit trap".to_string(),
            with_shell_name: true,
        }
    }
    pub fn signal_trap() -> Self {
        ErrorContext {
            label: "trap".to_string(),
            with_shell_name: true,
        }
    }
    pub fn sourced(path: &str) -> Self {
        ErrorContext {
            label: path.to_string(),
            with_shell_name: false,
        }
    }
}

pub struct Shell {
    pub env: HashMap<String, String>,
    pub cwd: String,
    pub oldpwd: Option<String>,
    pub positional: Vec<String>,
    pub last_status: i32,
    pub self_pid: i32,

    pub aliases: HashMap<String, String>,
    pub exported: HashSet<String>,
    pub readonly: HashSet<String>,
    pub integer_vars: HashSet<String>,
    pub functions: HashMap<String, parser::CommandList>,
    pub options: SetOptions,
    pub dir_stack: Vec<String>,
    pub traps: HashMap<String, String>,
    pub hash_table: HashMap<String, String>,
    /// How often every hashed command was executed (the `hits' column of
    /// the `hash' listing).
    pub hash_hits: HashMap<String, u32>,
    pub disabled_builtins: HashSet<String>,

    /// 动态插件注册表（进程全局状态的 per-Shell 快照，见 crate::plugin）。
    /// `disabled_builtins` 保持 per-Shell 语义，与此处的插件注册表正交。
    pub plugins: plugin::PluginState,

    /// Exported variables that have no value at all (`export NAME` without an
    /// assignment). bash prints them as `declare -x NAME` instead of
    /// `declare -x NAME=""`.
    pub null_exported: HashSet<String>,

    /// The value of `$0`: the path the shell itself was invoked with.
    /// `source` does NOT change it -- bash keeps the shell's own name
    /// while a file is sourced.
    pub dollar_zero: String,

    /// Set by `exec` when it is used with redirections only: the redirections
    /// must survive the builtin instead of being undone.
    pub persistent_redirs: bool,

    /// True while the interactive REPL is running. Bash only expands aliases
    /// in interactive shells unless the `expand_aliases` shell option is set.
    pub interactive: bool,

    /// Startup flags reported by the special parameter `$-`.  Unlike the
    /// single-letter `set` options (read fresh from `self.options` on every
    /// expansion), these three are fixed when the shell is invoked and never
    /// change during the session.
    ///
    /// `command_string` is the `c` letter (`-c` / `--command`), `reading_stdin`
    /// is the `s` letter (commands read from stdin: `-s` or a shell with no
    /// `-c` and no script file), and `dash_interactive` is the `i` letter.
    /// `dash_interactive` is deliberately separate from `self.interactive`:
    /// wash turns the latter on even for piped (non-tty) input, while bash's
    /// `$-` must NOT show `i` in that case.
    pub command_string: bool,
    pub reading_stdin: bool,
    pub dash_interactive: bool,

    /// The `expand_aliases` shell option (bash's `shopt -s expand_aliases`).
    /// When it is on, aliases are expanded even in non-interactive shells.
    pub expand_aliases: bool,

    /// The `sourcepath` shell option (bash's `shopt -u sourcepath`).  It is
    /// ON by default: `source` then looks for a file without a slash in
    /// `$PATH` FIRST and in the current directory afterwards.  With
    /// `shopt -u sourcepath` the current directory comes first.
    pub sourcepath: bool,

    /// Set by `return` so the enclosing `source`/function can stop.
    pub return_requested: Option<i32>,
    /// Set by `break`/`continue` (loop context owned by Phase 3).
    pub break_requested: Option<i32>,
    pub continue_requested: Option<i32>,
    /// Set by `exit` so the caller can terminate with the right code.
    pub exit_requested: Option<i32>,

    /// How many `source`/`.` invocations are currently active. `return` is
    /// only legal inside a sourced script (or a function) — the depth lets the
    /// `return` builtin tell a top-level call from a sourced one.
    pub sourcing_depth: usize,
    /// How many `for`/`while`/`until` loop bodies are currently executing.
    /// `break`/`continue` are only legal inside a loop.
    pub loop_depth: usize,
    /// How many shell-function calls are currently active. `local` is only
    /// legal inside a function.
    pub function_depth: usize,
    /// The stack of local-variable scopes.  `execute_function` pushes a frame
    /// for each call and the `local` builtin records, in the top frame, the
    /// previous value of every variable it makes local so the enclosing scope
    /// can be restored on return.  A frame maps a variable name to `Some(prev)`
    /// (it had a value) or `None` (it was unset).
    pub call_stack: Vec<HashMap<String, Option<String>>>,

    /// The pid of the most recently started background job (`$!`).
    pub bg_pid: i32,
    /// Background job pids this shell has started and not yet reaped (`wait`).
    pub jobs: Vec<i32>,
    /// 与 `jobs` 平行的真实作业表：保存 pid、命令文本与状态，供
    /// `jobs` / `fg` / `bg` / `disown` 定位与展示。`wait` 只操作 `jobs`，
    /// 回收时会同步清理这里的对应条目。
    pub job_table: Vec<JobInfo>,
    /// Array variables (`ARR=(a b)`). Each maps a name to an ordered list of
    /// elements (bash arrays).
    pub arrays: HashMap<String, Vec<String>>,
    /// Indices actually assigned in each indexed array.  `a[2]=x` must be a
    /// one-element sparse array in bash; without this set the hole filler
    /// would be counted as two extra elements by `${#a[@]}` / `${!a[@]}`.
    pub array_set_indices: HashMap<String, HashSet<i64>>,
    /// Offset (in characters) inside `positional[OPTIND-1]` where the next
    /// `getopts` call resumes a bundled option cluster (`-ab`), and the
    /// position index the offset belongs to.  bash keeps the same state
    /// across separate `getopts` invocations.
    pub getopts_offset: (usize, usize),
    /// Associative arrays (`declare -A m; m[k]=v`).  Kept separate from
    /// `arrays` so the indexed-array fast paths stay unchanged.  Each entry
    /// stores insertion-ordered `(key, value)` pairs; `${!m[@]}` must be
    /// rendered in bash's hash-bucket order (see `assoc_key_order`).
    pub assoc_arrays: HashMap<String, Vec<(String, String)>>,
    /// Names declared associative by `declare -A` even when still empty.
    pub assoc_declared: HashSet<String>,
    /// 记录被显式赋予过数组值（哪怕是空数组）的变量名。bash 区分「仅
    /// `declare -a X` 声明」与「`X=()` / `mapfile` 赋过值」：前者
    /// `declare -p` 打印 `declare -a X`，后者打印 `declare -a X=()`。
    /// wash 用连续 `Vec` 存数组，无法从空 `Vec` 本身区分这两种状态，
    /// 故另设此集合，由真正赋值数组的内建（`mapfile`/`readarray`）维护。
    pub array_assigned: HashSet<String>,
    /// 可编程补全规格表（`complete`）：按注册顺序保存
    /// `(命令名, 可回显的选项串)`。`complete -p` / 无参 `complete` 据此回显，
    /// 不写死空输出。
    pub completion_specs: Vec<(String, String)>,
    /// The interactive / non-interactive command history (`history`, `fc`).
    pub history: Vec<String>,
    /// A fatal error that aborts the rest of a `-c` script the way bash does
    /// after an assignment to a readonly variable. The value is the exit code.
    pub fatal_error: Option<i32>,

    /// True while a profile file is being sourced: the shell's own parse and
    /// lex errors are then suppressed.
    ///
    /// bash 的 profile 文件是用合法的 bash 语法写的，bash 读它们不会报错；
    /// wash 的语法覆盖还不全，读同一个文件可能失败。这种失败是 wash 自身的
    /// 限制，不应该混进命令的输出里，所以这里把错误静音（bash 在同样的情形
    /// 下也是"干净"的输出）。
    pub quiet_errors: bool,

    /// Name of the input source currently being parsed. bash puts it in
    /// every parse-error prefix, and it changes for nested `run_str` calls
    /// (`eval`, `source`, trap actions). `None` is the interactive REPL,
    /// where bash prints a plain one-line error with no line number.
    pub error_context: Option<ErrorContext>,
}

impl Shell {
    pub fn new() -> Self {
        // 继承的环境是「外部文本 ingress」：字面 U+E100 必须双写，否则
        // decode_raw 会把紧邻的 U+E000 误当编码字节。与 -c / 脚本 / source
        // 三个源码入口同一纪律。
        let mut env: HashMap<String, String> = std::env::vars()
            .map(|(k, v)| {
                (
                    crate::byteval::escape_literal(&k),
                    crate::byteval::escape_literal(&v),
                )
            })
            .collect();

        // bash increments SHLVL on every shell startup.  This happens
        // EXACTLY ONCE per process, here -- and NOT in `from_env`, which also
        // builds the state of a forked child: a subshell must not increment
        // SHLVL a second time (bash keeps the parent's value there).
        let inherited = env
            .get("SHLVL")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        env.insert("SHLVL".to_string(), (inherited.max(0) + 1).to_string());

        // `$_` starts out as the shell's own path and is exported.  As soon
        // as a command runs, bash replaces it with that command's last
        // argument and drops its export attribute -- see
        // `executor::update_underscore`.
        if !env.contains_key("_") {
            env.insert("_".to_string(), shell_self_path());
        }

        let mut sh = Shell::from_env(env);
        sh.exported.insert("SHLVL".to_string());
        sh.exported.insert("_".to_string());
        sh
    }

    /// Build a shell from a given environment map (used for command
    /// substitution subshells and `-c` invocations). All inherited variables
    /// are considered exported (matching bash, where the startup environment
    /// is exported by default).
    pub fn from_env(env: HashMap<String, String>) -> Self {
        let mut env = env;
        // bash pre-sets IFS as a shell variable (space, tab and newline).
        // It is not exported unless the environment says so.
        let ifs_defaulted = !env.contains_key("IFS");
        if ifs_defaulted {
            env.insert("IFS".to_string(), " \t\n".to_string());
        }
        // bash keeps `$PWD' in sync with the working directory; the logical
        // path from the environment wins over the physical one so that
        // symlinked directories survive `cd`.
        let cwd = match env.get("PWD") {
            Some(p) if p.starts_with('/') => p.clone(),
            _ => {
                let physical = crate::posix::getcwd();
                if physical.is_empty() {
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| "/".to_string())
                } else {
                    physical
                }
            }
        };
        let mut exported: HashSet<String> = env.keys().cloned().collect();
        if ifs_defaulted {
            exported.remove("IFS");
        }
        // NOTE: `SHLVL` and `$_` are deliberately NOT touched here.  This
        // constructor also builds the state of a forked child, and a child
        // must inherit the parent's `SHLVL` / `$_` values unchanged.  Both
        // are established once per process in `Shell::new`.
        let mut sh = Shell {
            env,
            cwd: cwd.clone(),
            oldpwd: None,
            positional: Vec::new(),
            last_status: 0,
            self_pid: std::process::id() as i32,
            aliases: HashMap::new(),
            exported,
            readonly: HashSet::new(),
            integer_vars: HashSet::new(),
            functions: HashMap::new(),
            options: SetOptions::default(),
            dir_stack: vec![cwd.clone()],
            traps: HashMap::new(),
            hash_table: HashMap::new(),
            hash_hits: HashMap::new(),
            disabled_builtins: HashSet::new(),
            plugins: plugin::PluginState::default(),
            null_exported: HashSet::new(),
            dollar_zero: "wash".to_string(),
            persistent_redirs: false,
            interactive: false,
            command_string: false,
            reading_stdin: false,
            dash_interactive: false,
            expand_aliases: false,
            sourcepath: true,
            return_requested: None,
            break_requested: None,
            continue_requested: None,
            exit_requested: None,
            sourcing_depth: 0,
            loop_depth: 0,
            function_depth: 0,
            call_stack: Vec::new(),
            bg_pid: 0,
            jobs: Vec::new(),
            job_table: Vec::new(),
            arrays: HashMap::new(),
            array_set_indices: HashMap::new(),
            getopts_offset: (0, 0),
            assoc_arrays: HashMap::new(),
            assoc_declared: HashSet::new(),
            array_assigned: HashSet::new(),
            completion_specs: Vec::new(),
            history: Vec::new(),
            fatal_error: None,
            quiet_errors: false,
            error_context: None,
        };
        Self::populate_internal_variables(&mut sh);
        sh
    }

    /// The value of `$-`: the single-letter shell option flags in bash's
    /// canonical order `a b e f h i k m p t u v x B C E H P T c s`, followed
    /// by the fixed startup letters `c` and `s`.  The mutable option letters
    /// are read fresh from `self.options` on every call, so `set` / `set +o`
    /// changes are reflected immediately instead of being cached in a stale
    /// string.  Measured against MSYS2 bash 5.3 (`set -abefhkmptuvxBCEHPT`
    /// yields `abefhkmptuvxBCEHPTc`).
    pub fn dash_flags(&self) -> String {
        let o = &self.options;
        let mut s = String::new();
        if o.allexport { s.push('a'); }
        if o.notify { s.push('b'); }
        if o.errexit { s.push('e'); }
        if o.noglob { s.push('f'); }
        if o.hashall { s.push('h'); }
        if self.dash_interactive { s.push('i'); }
        if o.keyword { s.push('k'); }
        if o.monitor { s.push('m'); }
        if o.privileged { s.push('p'); }
        if o.onecmd { s.push('t'); }
        if o.nounset { s.push('u'); }
        if o.verbose { s.push('v'); }
        if o.xtrace { s.push('x'); }
        if o.braceexpand { s.push('B'); }
        if o.noclobber { s.push('C'); }
        if o.errtrace { s.push('E'); }
        if o.histexpand { s.push('H'); }
        if o.physical { s.push('P'); }
        if o.functrace { s.push('T'); }
        if self.command_string { s.push('c'); }
        if self.reading_stdin { s.push('s'); }
        s
    }

    /// Install bash's internal variables that are not shell-instance specific
    /// (version strings, platform names, the empty array/option tables, ...).
    /// None of these are exported.  `$SHLVL` and `$_` are handled separately
    /// in `Shell::new` (both are exported; SHLVL is incremented once per
    /// process and `$_` loses its export attribute as soon as a command
    /// runs).
    fn populate_internal_variables(sh: &mut Shell) {
        // 动态插件注册表：Shell::new 与 from_env 两条构造路径的公共落点，
        // 在此一处注入全局快照即可覆盖全部构造路径。
        sh.plugins = plugin::current_state();
        // 版本号是编译期常量：用 concat! 在编译期拼好，省掉每次构造
        // Shell 时的一次 format! 堆分配。
        sh.env.insert(
            "BASH_VERSION".to_string(),
            concat!("wash ", env!("WASH_VERSION")).to_string(),
        );
        sh.env.insert("BASH".to_string(), sh.dollar_zero.clone());
        sh.env.insert("BASHOPTS".to_string(), String::new());
        sh.env.insert("SHELLOPTS".to_string(), String::new());
        sh.env.insert("HOSTTYPE".to_string(), "x86_64".to_string());
        sh.env
            .insert("MACHTYPE".to_string(), "x86_64-pc-cygwin".to_string());
        sh.env.insert("OSTYPE".to_string(), "cygwin".to_string());
        sh.env
            .insert("EUID".to_string(), posix::effective_uid().to_string());
        sh.env
            .insert("UID".to_string(), posix::real_uid().to_string());
        sh.env
            .insert("PPID".to_string(), posix::parent_pid().to_string());
        sh.env.insert("OPTIND".to_string(), "1".to_string());
        sh.env.insert("OPTERR".to_string(), "1".to_string());
        sh.env.insert("PS4".to_string(), "+ ".to_string());
        // Array-valued internal variables.
        sh.arrays.insert(
            "BASH_VERSINFO".to_string(),
            vec![
                "0".to_string(),
                "0".to_string(),
                "1".to_string(),
                "release".to_string(),
                "".to_string(),
                "x86_64-pc-cygwin".to_string(),
            ],
        );
        sh.arrays.insert("DIRSTACK".to_string(), vec![sh.cwd.clone()]);
        sh.arrays
            .insert("GROUPS".to_string(), posix::group_ids().iter().map(|g| g.to_string()).collect());
        sh.arrays.insert("BASH_ALIASES".to_string(), Vec::new());
        sh.arrays.insert("BASH_ARGC".to_string(), Vec::new());
        sh.arrays.insert("BASH_ARGV".to_string(), Vec::new());
        sh.arrays.insert("BASH_CMDS".to_string(), Vec::new());
        sh.arrays.insert("BASH_LINENO".to_string(), Vec::new());
        sh.arrays.insert("BASH_SOURCE".to_string(), Vec::new());
    }

    /// Build the environment map handed to a child process: only exported
    /// variables, plus always-present `PWD`.
    pub fn child_env(&self) -> HashMap<String, String> {
        let mut e = HashMap::new();
        for (k, v) in &self.env {
            if self.exported.contains(k) {
                e.insert(k.clone(), v.clone());
            }
        }
        // PWD is always exported (bash keeps it in sync).
        e.insert("PWD".to_string(), self.cwd.clone());
        e
    }

    /// Run a nested command string (`eval`, `source`, a trap action) with
    /// bash's matching parse-error prefix, restoring the caller's context
    /// afterwards. Without the save/restore a `-c` invocation would keep
    /// labelling every nested error as `-c:`.
    pub fn run_in_error_context(&mut self, ctx: ErrorContext, s: &str) -> i32 {
        let saved = self.error_context.replace(ctx);
        let code = self.run_str(s);
        self.error_context = saved;
        code
    }

    /// Report a lex/parse error the way bash does for the current input
    /// source. `bash -c CMD` prints:
    ///     bash: -c: line 1: syntax error near unexpected token `;'
    ///     bash: -c: line 1: `coproc; echo "rc=$?"'
    /// The label changes with the input source (`eval`, `exit trap`, `trap`,
    /// or the sourced script's path, which drops the shell-name prefix).
    /// End-of-input errors have a single line and no echo. The diff harness
    /// strips only the shell-name prefix, so the label, line number and echo
    /// line all have to match byte-for-byte.
    /// Number of source lines consumed by here-document bodies / terminators.
    /// bash reads the whole command line (including all heredoc bodies)
    /// before diagnosing a missing redirection target, so those lines add
    /// to the reported line number.
    fn heredoc_consumed_lines(tokens: &[lexer::Token]) -> usize {
        let mut total = 0usize;
        for t in tokens {
            if let lexer::Token::Redirect {
                heredoc: Some(hd), ..
            } = t
            {
                // A heredoc whose delimiter word is missing never starts
                // reading a body: collect_heredocs() stores an empty
                // placeholder with delim == "". Those lines must not be
                // counted (e.g. `cat <<` reports line 1, not line 2).
                if hd.delim.is_empty() && hd.text.is_empty() {
                    continue;
                }
                let body_lines = hd.text.lines().count();
                total += body_lines + if hd.eof { 0 } else { 1 };
            }
        }
        total
    }

    /// Largest `k` such that the first `k` lines form a complete command list.
    /// bash parses and executes line by line: commands that are already
    /// complete keep their output and side effects when a later line has a
    /// syntax error.  `tokenize_nested` suppresses parse-time heredoc warnings
    /// for these speculative prefix parses.
    fn largest_parseable_prefix(s: &str) -> usize {
        let lines: Vec<&str> = s.split('\n').collect();
        // The whole string has already failed to parse (this is the parse-error
        // recovery path), so the answer is strictly less than `lines.len()`.
        // Walk BACKWARDS and stop at the first prefix that parses: for the
        // overwhelmingly common "syntax error at the end of the script" case
        // that is one tokenize+parse instead of one per prefix.  The old
        // forward loop re-parsed every prefix (O(n²) tokenizations: 2000 lines
        // took ~39s here vs bash's ~0.25s).  "prefix(k) parses" is not monotone
        // in general (an unclosed `if` can close later), but the largest
        // satisfying k is identical whichever direction we scan.
        for k in (1..lines.len()).rev() {
            let prefix = lines[..k].join("\n");
            if lexer::tokenize_nested(&prefix)
                .ok()
                .and_then(|t| parser::parse_command_list(&t).ok())
                .is_some()
            {
                return k;
            }
        }
        0
    }

    /// Execute the completed prefix of a command string that failed to parse.
    fn execute_completed_prefix(&mut self, s: &str, k: usize) {
        if k == 0 {
            return;
        }
        let lines: Vec<&str> = s.split('\n').collect();
        let prefix = lines[..k].join("\n");
        if let Ok(tokens) = lexer::tokenize_nested(&prefix) {
            if let Ok(list) = parser::parse_command_list(&tokens) {
                executor::execute(&list, self);
            }
        }
    }

    /// Report a lex/parse error with bash's two line-numbering rules and
    /// execute any command lines that were already complete.
    ///
    ///   * `unexpected EOF ...` / `syntax error: unexpected end of file`
    ///     use the virtual line just past end of input (`lines + 1`).
    ///   * `syntax error near unexpected token ...` uses the real 1-based
    ///     line of the offending command, plus any heredoc body lines bash
    ///     had to read before diagnosing it, and echoes that line.
    fn report_command_error(
        &mut self,
        s: &str,
        tokens: Option<&[lexer::Token]>,
        e: &WashError,
    ) {
        if self.quiet_errors {
            return;
        }
        let lines: Vec<&str> = s.split('\n').collect();
        let k = Self::largest_parseable_prefix(s);
        self.execute_completed_prefix(s, k);

        // A structured `DiagnosticFamily` disambiguates diagnostics whose message
        // strings are byte-identical (e.g. an unclosed `$(` vs `$((`) yet need
        // different line numbers.  `Unspecified` keeps the legacy message-prefix
        // heuristic for every existing parser/executor error.
        // The third tuple item is an optional extra diagnostic: (line, text).
        // `CondExpr` adds bash's yacc-layer "syntax error near ..." line;
        // `CondRegexEof` adds the second line after an unclosed regex `(`.
        let (line_no, echo, extra): (usize, Option<&str>, Option<(usize, String)>) = match e.family {
            DiagnosticFamily::CmdsubEof => (s.lines().count() + 1, None, None),
            DiagnosticFamily::OtherDelimiterEof => (e.open_line, None, None),
            // Unterminated compound command: bash reports the virtual line
            // just past the input (n_lines + 1), no echo; the message itself
            // already carries "on line N" for the innermost opener (qa9).
            DiagnosticFamily::CompoundEof => (s.lines().count() + 1, None, None),
            // A malformed `[[ ... ]]`: bash prints the conditional parser's
            // own message, then "syntax error near <token>", then echoes the
            // source line holding the `[[`.  `open_line` is that line.
            DiagnosticFamily::CondExpr => {
                let ln = e.open_line;
                let src = lines.get(ln.saturating_sub(1)).copied();
                (
                    ln,
                    src,
                    Some((ln, format!("syntax error near `{}'", e.near))),
                )
            }
            // Unmatched `( ` in the `=~` RHS: the lexer error names the
            // opener's line, then bash adds a second line on the virtual line
            // past end of input.
            DiagnosticFamily::CondRegexEof => (
                e.open_line,
                None,
                Some((
                    s.lines().count() + 1,
                    "unexpected argument to conditional binary operator".to_string(),
                )),
            ),
            DiagnosticFamily::NearToken => {
                let consumed = tokens.map(Self::heredoc_consumed_lines).unwrap_or(0);
                (
                    k + 1 + consumed,
                    Some(lines.get(k).copied().unwrap_or("")),
                    None,
                )
            }
            DiagnosticFamily::Unspecified => {
                let at_eof = e.message.starts_with("syntax error: unexpected end of file")
                    || e.message.starts_with("unexpected EOF");
                let near_token = e.message.starts_with("syntax error near unexpected token");
                if at_eof {
                    (s.lines().count() + 1, None, None)
                } else if near_token {
                    let consumed = tokens.map(Self::heredoc_consumed_lines).unwrap_or(0);
                    (
                        k + 1 + consumed,
                        Some(lines.get(k).copied().unwrap_or("")),
                        None,
                    )
                } else {
                    (1, None, None)
                }
            }
        };
        self.report_parse_error(&e.message, line_no, echo, extra);
    }

    fn report_parse_error(
        &self,
        message: &str,
        line_no: usize,
        echo: Option<&str>,
        extra: Option<(usize, String)>,
    ) {
        if self.quiet_errors {
            return;
        }
        let ctx = match &self.error_context {
            Some(c) => c,
            None => {
                // Interactive REPL: bash prints a plain one-liner with no
                // line number and no echo of the input.
                eprintln!("wash: {}", message);
                return;
            }
        };
        let at_eof = message.starts_with("syntax error: unexpected end of file")
            || message.starts_with("unexpected EOF");
        let prefix = if ctx.with_shell_name {
            format!("wash: {}: line {}: ", ctx.label, line_no)
        } else {
            format!("{}: line {}: ", ctx.label, line_no)
        };
        // bash names the line where the unterminated compound started:
        // `syntax error: unexpected end of file from `{' command on line 1`.
        let body = if at_eof && message.ends_with(" command") {
            format!("{} on line 1", message)
        } else {
            message.to_string()
        };
        // An empty message means bash produced no conditional-parser line
        // (a bare `[[ ]]`), so only the yacc "near" line and echo are printed.
        if !message.is_empty() {
            eprintln!("{}{}", prefix, body);
        }
        if let Some((extra_line, extra_msg)) = extra {
            let extra_prefix = if ctx.with_shell_name {
                format!("wash: {}: line {}: ", ctx.label, extra_line)
            } else {
                format!("{}: line {}: ", ctx.label, extra_line)
            };
            eprintln!("{}{}", extra_prefix, extra_msg);
        }
        if let Some(line) = echo {
            eprintln!("{}`{}'", prefix, line);
        }
    }

    /// Run a single command string (used by `-c` and the interactive loop).
    pub fn run_str(&mut self, s: &str) -> i32 {
        // 顶层命令开始即清掉上一轮可能残留的退出/返回标志，避免 `exit` 等内建设置的
        // 标志跨命令泄漏、导致后续命令只执行第一个 and-or 列表（code-review P1-#20）。
        self.exit_requested = None;
        // A fatal arithmetic expansion is scoped to one command line; the
        // interactive shell must remain usable on the next line.
        crate::expand::clear_arith_failed();
        crate::expand::clear_expansion_abort();
        let code = match lexer::tokenize(s) {
            Ok(tokens) => match parser::parse_command_list(&tokens) {
                Ok(list) => {
                    let c = executor::execute(&list, self);
                    self.last_status = c;
                    c
                }
                Err(e) => {
                    self.report_command_error(s, Some(&tokens), &e);
                    2
                }
            },
            Err(e) => {
                self.report_command_error(s, None, &e);
                2
            }
        };
        // A fatal parameter expansion terminates a non-interactive shell with
        // 127 (bash does the same), but an interactive REPL must survive:
        // only this command line is aborted and its status becomes 127.
        if crate::expand::expansion_aborted() {
            if !self.interactive {
                std::process::exit(127);
            }
            self.last_status = 127;
            return 127;
        }
        self.last_status = code;
        code
    }

    /// Finalise a non-interactive (`-c` / script) invocation: run the EXIT
    /// trap and return the process exit code bash would use.
    ///
    /// The `exit` builtin (including one executed from a signal trap) records
    /// the requested status in `exit_requested`, while `run_str` returns only
    /// the status of the last command.  A signal trap such as
    /// `trap 'exit 42' INT; kill -INT $$` therefore leaves
    /// `exit_requested = Some(42)` but `run_str` returns 0; bash exits with
    /// 42, so the explicit request must win over the command status.
    ///
    /// The EXIT trap runs after the command finished.  Its own `exit N`
    /// overrides the status (`trap 'exit 42' EXIT; exit 7` -> 42); when it
    /// does not call `exit`, the earlier request is kept.  `run_str` clears
    /// `exit_requested` on entry, so we clear it ourselves before dispatching
    /// the trap in order to detect a fresh `exit` inside the trap.
    pub fn finalize_exit(&mut self, command_status: i32) -> i32 {
        let explicit = self.exit_requested;
        self.exit_requested = None;
        if let Some(action) = self.traps.get("EXIT").cloned() {
            if action != "-" {
                self.run_in_error_context(ErrorContext::exit_trap(), &action);
            }
        }
        let trap_exit = self.exit_requested;
        let code = trap_exit.or(explicit).unwrap_or(command_status);
        // Process exit codes are a single byte, as on the interactive path.
        code.rem_euclid(256)
    }

    pub fn run_interactive(&mut self) {
        // Aliases are only expanded in interactive shells (bash's
        // `expand_aliases` shell option is off by default elsewhere).
        self.interactive = true;
        crate::repl::run(self);
    }
}
