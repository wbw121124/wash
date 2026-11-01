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
    pub functions: HashMap<String, String>,
    pub options: SetOptions,
    pub dir_stack: Vec<String>,
    pub traps: HashMap<String, String>,
    pub hash_table: HashMap<String, String>,
    pub disabled_builtins: HashSet<String>,

    /// Exported variables that have no value at all (`export NAME` without an
    /// assignment). bash prints them as `declare -x NAME` instead of
    /// `declare -x NAME=""`.
    pub null_exported: HashSet<String>,

    /// The value of `$0`. `source` replaces it with the script path while the
    /// script runs.
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
}

impl Shell {
    pub fn new() -> Self {
        let env: HashMap<String, String> = std::env::vars().collect();
        Shell::from_env(env)
    }

    /// Build a shell from a given environment map (used for command
    /// substitution subshells and `-c` invocations). All inherited variables
    /// are considered exported (matching bash, where the startup environment
    /// is exported by default).
    pub fn from_env(env: HashMap<String, String>) -> Self {
        // bash keeps `$PWD` in sync with the working directory; the logical
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
        let exported: HashSet<String> = env.keys().cloned().collect();
        Shell {
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
        }
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
                    eprintln!("wash: {}", e);
                    2
                }
            },
            Err(e) => {
                eprintln!("wash: {}", e);
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
