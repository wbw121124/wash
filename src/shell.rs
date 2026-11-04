// Shell state + REPL driver.
//
// Holds the environment, positional parameters, last exit status, and the
// current directory. `run_str` is the single command-execution entry point
// shared by `-c` and the interactive REPL (in `repl.rs`). The interactive
// fish-style line editor lives entirely in `repl.rs`; this module only owns
// the persistent shell state and dispatches to the executor.

use std::collections::{HashMap, HashSet};

use crate::executor;
use crate::lexer;
use crate::parser;
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
            "igncr" => self.igncr,
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
            "igncr" => self.igncr = value,
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
/// readonly / integer attributes. A forked child needs them so that
/// `export -p` prints the same declarations as in the parent.
#[derive(Clone, Debug, Default)]
pub struct VarAttrs {
    pub exported: HashSet<String>,
    pub null_exported: HashSet<String>,
    pub readonly: HashSet<String>,
    pub integer_vars: HashSet<String>,
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
    /// Array variables (`ARR=(a b)`). Each maps a name to an ordered list of
    /// elements (bash arrays).
    pub arrays: HashMap<String, Vec<String>>,
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
}

impl Shell {
    pub fn new() -> Self {
        let mut env: HashMap<String, String> = std::env::vars().collect();

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
            null_exported: HashSet::new(),
            dollar_zero: "wash".to_string(),
            persistent_redirs: false,
            interactive: false,
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
            arrays: HashMap::new(),
            history: Vec::new(),
            fatal_error: None,
            quiet_errors: false,
        };
        Self::populate_internal_variables(&mut sh);
        sh
    }

    /// Install bash's internal variables that are not shell-instance specific
    /// (version strings, platform names, the empty array/option tables, ...).
    /// None of these are exported.  `$SHLVL` and `$_` are handled separately
    /// in `Shell::new` (both are exported; SHLVL is incremented once per
    /// process and `$_` loses its export attribute as soon as a command
    /// runs).
    fn populate_internal_variables(sh: &mut Shell) {
        sh.env
            .insert("BASH_VERSION".to_string(), "wash 0.0.1".to_string());
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

    /// The variable attributes a subshell must inherit.
    pub fn attrs(&self) -> VarAttrs {
        VarAttrs {
            exported: self.exported.clone(),
            null_exported: self.null_exported.clone(),
            readonly: self.readonly.clone(),
            integer_vars: self.integer_vars.clone(),
        }
    }

    /// Copy the parent's variable attributes into a subshell.
    pub fn apply_attrs(&mut self, attrs: &VarAttrs) {
        self.exported = attrs.exported.clone();
        self.null_exported = attrs.null_exported.clone();
        self.readonly = attrs.readonly.clone();
        self.integer_vars = attrs.integer_vars.clone();
    }

    /// Run a single command string (used by `-c` and the interactive loop).
    pub fn run_str(&mut self, s: &str) -> i32 {
        let code = match lexer::tokenize(s) {
            Ok(tokens) => match parser::parse_command_list(&tokens) {
                Ok(list) => {
                    let c = executor::execute(&list, self);
                    self.last_status = c;
                    c
                }
                Err(e) => {
                    if !self.quiet_errors {
                        eprintln!("wash: {}", e);
                    }
                    2
                }
            },
            Err(e) => {
                if !self.quiet_errors {
                    eprintln!("wash: {}", e);
                }
                2
            }
        };
        self.last_status = code;
        code
    }

    pub fn run_interactive(&mut self) {
        // Aliases are only expanded in interactive shells (bash's
        // `expand_aliases` shell option is off by default elsewhere).
        self.interactive = true;
        crate::repl::run(self);
    }
}
