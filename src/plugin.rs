// 二进制插件系统：宿主注册表与 C ABI 桥接。
//
//! 头文件 `include/wash_plugin.h` 定义稳定 C ABI（v1）。本模块提供：
//!   * 与头文件逐字段一致的 `#[repr(C)]` 结构；
//!   * `PluginState` / `DynamicBuiltin` 注册表 —— 所有 CStr 立即拷贝成
//!     Rust `String`、函数地址存成 `usize`，宿主绝不留存任何指向 .so 数据段的
//!     裸指针，因此卸载后不依赖已失效的库内存；
//!   * 进程全局 `OnceLock<Mutex<PluginState>>`，Shell 构造时取快照；
//!   * 宿主 `extern "C"` 回调（vtable），从 `ctx.shell` 短暂还原 `&mut Shell`；
//!   * `load_plugin` / `unload_plugin` / `invoke` 与统一名单 API。
//
// 平台耦合只走 posix::dl_* -> posix_shim.c 的 wash_dl*；本文件不声明任何
// raw libc / unix-only crate。

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::posix;
use crate::shell::Shell;

/// ABI 版本，与 `include/wash_plugin.h` 的 `WASH_PLUGIN_ABI_VERSION` 一致。
pub const ABI_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// C ABI (v1) —— 必须与 include/wash_plugin.h 逐字段一致
// ---------------------------------------------------------------------------

/// 头文件只暴露 opaque `typedef struct WashPluginCtx WashPluginCtx;`；
/// 这里是宿主私有定义。`tmp` 是 `get_var` 的 out 缓冲（4KB）。
#[repr(C)]
pub struct WashPluginCtx {
    pub shell: *mut c_void, // -> Shell
    pub api: *const WashPluginApi,
    pub tmp: [c_char; 4096],
}

type PrintFn = unsafe extern "C" fn(*mut WashPluginCtx, *const c_char) -> c_int;
type GetVarFn =
    unsafe extern "C" fn(*mut WashPluginCtx, *const c_char, *mut *const c_char) -> c_int;
type SetVarFn = unsafe extern "C" fn(*mut WashPluginCtx, *const c_char, *const c_char) -> c_int;
type IsEnabledFn = unsafe extern "C" fn(*mut WashPluginCtx, *const c_char) -> c_int;
type OnUnloadFn = unsafe extern "C" fn();

/// `typedef int (*WashBuiltinFn)(int, char **, WashPluginCtx *)`。
pub type WashBuiltinFn =
    unsafe extern "C" fn(c_int, *mut *mut c_char, *mut WashPluginCtx) -> c_int;
/// `int wash_plugin_init(const WashPluginApi *, uint32_t, WashPluginInfo *)`。
pub type PluginInitFn =
    unsafe extern "C" fn(*const WashPluginApi, u32, *mut WashPluginInfo) -> c_int;

/// `WashPluginApi`：宿主拥有的 vtable。函数指针用 `Option<fn>`（nullable
/// pointer optimization 保证布局与 C 的裸函数指针完全一致）。
#[repr(C)]
pub struct WashPluginApi {
    pub abi_version: u32,
    pub struct_size: u32,
    pub reserved: *mut c_void,
    pub print: Option<PrintFn>,
    pub eprint: Option<PrintFn>,
    pub get_var: Option<GetVarFn>,
    pub set_var: Option<SetVarFn>,
    pub is_enabled: Option<IsEnabledFn>,
}

// WashPluginApi 只含函数指针与一个恒为 NULL 的 reserved 指针，跨线程只读、
// 从不通过 shared 引用做可变访问，故 Sync 成立（static 要求 T: Sync）。
unsafe impl Sync for WashPluginApi {}

/// `WashBuiltinDesc { name, fn, help, short_doc }`。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WashBuiltinDesc {
    pub name: *const c_char,
    pub fn_ptr: Option<WashBuiltinFn>,
    pub help: *const c_char,
    pub short_doc: *const c_char,
}

/// `WashPluginInfo { abi_version, struct_size, flags, builtins, count,
///  plugin_name, on_unload }`。
#[repr(C)]
pub struct WashPluginInfo {
    pub abi_version: u32,
    pub struct_size: u32,
    pub flags: u32,
    pub builtins: *const WashBuiltinDesc,
    pub count: u32,
    pub plugin_name: *const c_char,
    pub on_unload: Option<OnUnloadFn>,
}

impl WashPluginInfo {
    fn zeroed() -> Self {
        WashPluginInfo {
            abi_version: 0,
            struct_size: 0,
            flags: 0,
            builtins: std::ptr::null(),
            count: 0,
            plugin_name: std::ptr::null(),
            on_unload: None,
        }
    }
}

/// 宿主 vtable。地址在整个进程生命周期有效，插件可以缓存 `api` 指针或其中
/// 的函数指针（头文件 HARD CONTRACT 已向插件作者说明）。
static HOST_API: WashPluginApi = WashPluginApi {
    abi_version: ABI_VERSION,
    struct_size: std::mem::size_of::<WashPluginApi>() as u32,
    reserved: std::ptr::null_mut(),
    print: Some(cb_print),
    eprint: Some(cb_eprint),
    get_var: Some(cb_get_var),
    set_var: Some(cb_set_var),
    is_enabled: Some(cb_is_enabled),
};

// ---------------------------------------------------------------------------
// 宿主 extern "C" 回调（vtable 实现）
// ---------------------------------------------------------------------------

unsafe extern "C" fn cb_print(_ctx: *mut WashPluginCtx, text: *const c_char) -> c_int {
    if text.is_null() {
        return -1;
    }
    let s = CStr::from_ptr(text).to_string_lossy();
    print!("{}", s);
    let _ = std::io::stdout().flush();
    0
}

unsafe extern "C" fn cb_eprint(_ctx: *mut WashPluginCtx, text: *const c_char) -> c_int {
    if text.is_null() {
        return -1;
    }
    let s = CStr::from_ptr(text).to_string_lossy();
    eprint!("{}", s);
    let _ = std::io::stderr().flush();
    0
}

unsafe extern "C" fn cb_get_var(
    ctx: *mut WashPluginCtx,
    name: *const c_char,
    out: *mut *const c_char,
) -> c_int {
    if ctx.is_null() || name.is_null() || out.is_null() {
        return -1;
    }
    let shell_ptr = (*ctx).shell as *mut Shell;
    if shell_ptr.is_null() {
        return -1;
    }
    let sh: &mut Shell = &mut *shell_ptr;
    let key = CStr::from_ptr(name).to_string_lossy();
    match sh.env.get(key.as_ref()) {
        Some(v) => {
            // 把值拷进 ctx.tmp；out 只在下一次 API 调用前有效（头文件约定）。
            let c: &mut WashPluginCtx = &mut *ctx;
            let bytes = v.as_bytes();
            let n = bytes.len().min(c.tmp.len() - 1);
            for (i, &b) in bytes.iter().take(n).enumerate() {
                c.tmp[i] = b as c_char;
            }
            c.tmp[n] = 0;
            *out = c.tmp.as_ptr();
            0
        }
        None => -1,
    }
}

unsafe extern "C" fn cb_set_var(
    ctx: *mut WashPluginCtx,
    name: *const c_char,
    value: *const c_char,
) -> c_int {
    if ctx.is_null() || name.is_null() || value.is_null() {
        return -1;
    }
    let shell_ptr = (*ctx).shell as *mut Shell;
    if shell_ptr.is_null() {
        return -1;
    }
    let sh: &mut Shell = &mut *shell_ptr;
    let k = CStr::from_ptr(name).to_string_lossy().into_owned();
    let v = CStr::from_ptr(value).to_string_lossy().into_owned();
    sh.env.insert(k, v);
    0
}

unsafe extern "C" fn cb_is_enabled(ctx: *mut WashPluginCtx, name: *const c_char) -> c_int {
    if ctx.is_null() || name.is_null() {
        return 0;
    }
    let shell_ptr = (*ctx).shell as *mut Shell;
    if shell_ptr.is_null() {
        return 0;
    }
    let sh: &mut Shell = &mut *shell_ptr;
    let k = CStr::from_ptr(name).to_string_lossy();
    if sh.disabled_builtins.contains(k.as_ref()) {
        return 0;
    }
    if crate::builtins::is_builtin(sh, k.as_ref()) {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// 注册表
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct DynamicBuiltin {
    pub name: String,
    pub fn_addr: usize,
    pub handle: i32,
    pub plugin_path: String,
    pub help: Option<String>,
    pub short_doc: Option<String>,
    pub on_unload: usize,
}

#[derive(Clone, Default)]
pub struct PluginState {
    pub dynamic: HashMap<String, DynamicBuiltin>,
    /// plugin_path (realpath) -> dl shim 句柄槽位。
    pub handles: HashMap<String, i32>,
    pub whitelist: Vec<PathBuf>,
}

static PLUGIN_STATE: OnceLock<Mutex<PluginState>> = OnceLock::new();

fn global() -> &'static Mutex<PluginState> {
    PLUGIN_STATE.get_or_init(|| Mutex::new(PluginState::default()))
}

/// 当前全局注册表的快照 clone（供 Shell 构造、REPL 补全使用）。
pub fn current_state() -> PluginState {
    match global().lock() {
        Ok(g) => g.clone(),
        Err(e) => e.into_inner().clone(),
    }
}

/// 把某个 Shell 的注册表写回全局（fork/命令替换构造的新 Shell 会读到它）。
fn sync_global(state: &PluginState) {
    let mut g = match global().lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    *g = state.clone();
}

/// 统一名单：静态内建 + 动态内建名（静态保持原顺序，动态追加）。
pub fn all_builtin_names(sh: &Shell) -> Vec<String> {
    let mut v: Vec<String> = crate::builtins::BUILTINS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for k in sh.plugins.dynamic.keys() {
        if !v.iter().any(|x| x == k) {
            v.push(k.clone());
        }
    }
    v
}

/// 查动态内建（返回 clone，避免跨 FFI 借用）。
pub fn lookup_dynamic_builtin(sh: &Shell, name: &str) -> Option<DynamicBuiltin> {
    sh.plugins.dynamic.get(name).cloned()
}

// ---------------------------------------------------------------------------
// 白名单（兼容优先 realpath 校验）
// ---------------------------------------------------------------------------

/// oracle 默认 BASH_LOADABLES_PATH（5 段绝对目录 + `.`；`.`/空/相对段丢弃）。
const DEFAULT_LOADABLES_PATH: &str =
    "/usr/local/lib/bash:/usr/lib/bash:/opt/local/lib/bash:/usr/pkg/lib/bash:/opt/pkg/lib/bash:.";

/// 解析白名单：读 `BASH_LOADABLES_PATH`，未设用默认值；丢弃空段、`.`、
/// 以及所有不以 `/` 开头的相对段（拒绝相对路径逃逸）。
pub fn resolve_whitelist(sh: &Shell) -> Vec<PathBuf> {
    let raw = sh
        .env
        .get("BASH_LOADABLES_PATH")
        .map(|s| s.as_str())
        .unwrap_or(DEFAULT_LOADABLES_PATH);
    let mut out: Vec<PathBuf> = Vec::new();
    for seg in raw.split(':') {
        if seg.is_empty() || seg == "." || !seg.starts_with('/') {
            continue;
        }
        let p = PathBuf::from(seg);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn norm_dir(s: &str) -> String {
    let t = s.trim_end_matches('/');
    if t.is_empty() {
        "/".to_string()
    } else {
        t.to_string()
    }
}

/// 取 MSYS 风格路径的父目录（纯字符串，避开 Windows/Cygwin Path 语义差异）。
fn parent_of(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => "",
    }
}

/// 校验并归一化插件路径，返回 `(realpath, 校验时目标文件的 (dev, ino))`。
///
/// # 威胁模型与偏离说明（review #22 / #23）
///
/// * **#22（显式路径白名单）**：oracle（MSYS2 bash 5.3.20）实测
///   `enable -f <任意路径>` 一律 rc=2 报
///   ``enable: dynamic loading not available``——oracle 构建根本没有启用
///   动态加载，所以「显式路径是否受 BASH_LOADABLES_PATH 约束」在本平台
///   上没有可对齐的 oracle 行为。wash 的插件系统是自带扩展能力，显式
///   路径白名单因此是一项**未登记的安全加固（待主理人裁决：保留并登记
///   为有意偏离，或放开对齐上游 bash 源码行为）**；在裁决落地前行为保持
///   不变，此处留档防止再次静默偏离。
/// * **#23（TOCTOU）**：lstat 拒 symlink 与 `dl_open` 之间存在窗口。防护
///   分两层：(1) 这里把校验时目标文件的 `(dev, ino)` 一并返回；(2)
///   `load_plugin` 在 `dl_open` 成功后重新 `stat` 比对 inode，窗口内文件
///   被替换即拒绝。前提威胁模型：对白名单目录**有写权限**的攻击者本就
///   能直接决定被加载的内容（校验前替换不在防护目标内），本防护只封
///   「校验通过后、加载完成前」的替换窗口。
fn validate_path(path: &str, sh: &Shell) -> Result<(String, i64, i64), String> {
    let wl = resolve_whitelist(sh);
    if wl.is_empty() {
        return Err("BASH_LOADABLES_PATH has no usable absolute directory".to_string());
    }

    // 裸文件名按目录顺序搜索；含 `/` 的路径直接作为候选（兼容优先）。
    let candidate: String = if path.contains('/') {
        path.to_string()
    } else {
        let mut found = None;
        for d in &wl {
            let p = format!("{}/{}", d.to_string_lossy(), path);
            if posix::stat_path(&p, true).is_some() {
                found = Some(p);
                break;
            }
        }
        found.ok_or_else(|| format!("{}: not found in BASH_LOADABLES_PATH", path))?
    };

    // lstat 拒绝符号链接（follow=false -> 描述链接本身）。
    match posix::stat_path(&candidate, false) {
        Some(st) if st.file_type == posix::FT_LNK => {
            return Err(format!("{}: symbolic links are not allowed", path));
        }
        Some(_) => {}
        None => return Err(format!("{}: cannot stat", path)),
    }

    let real = posix::realpath(&candidate);
    if real.is_empty() {
        return Err(format!("{}: cannot resolve path", path));
    }

    // realpath 后父目录必须命中白名单（含 `/` 的路径也允许，但父目录要命中）。
    let parent = norm_dir(parent_of(&real));
    let ok = wl.iter().any(|d| {
        let ds = d.to_string_lossy();
        let rp = posix::realpath(ds.as_ref());
        let norm = if rp.is_empty() {
            norm_dir(ds.as_ref())
        } else {
            norm_dir(&rp)
        };
        norm == parent
    });
    if !ok {
        return Err(format!("{}: outside BASH_LOADABLES_PATH whitelist", path));
    }

    // 记录最终目标文件的 (dev, ino)，供 load_plugin 在 dl_open 之后复核
    //（review #23：封死校验 → 加载之间的 TOCTOU 窗口）。
    let st = posix::stat_path(&real, true)
        .ok_or_else(|| format!("{}: cannot stat", path))?;
    Ok((real, st.dev, st.ino))
}

/// 立即拷贝一个 C 字符串成 Rust String（NULL -> None）。
unsafe fn cstr_copy(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}

// ---------------------------------------------------------------------------
// 加载 / 卸载 / 调用
// ---------------------------------------------------------------------------

/// 加载插件。NAME 参数仅为 bash 语法兼容：wash ABI 由插件自带 builtins 列表
/// 自描述，一次 `-f` 注册插件声明的**全部** count 个内建。
pub fn load_plugin(path: &str, _names: &[String], sh: &mut Shell) -> Result<(), String> {
    let (real, dev0, ino0) = validate_path(path, sh)?;

    // 重复路径复用句柄。
    let (handle, is_new) = match sh.plugins.handles.get(&real) {
        Some(&h) => (h, false),
        None => (posix::dl_open(&real)?, true),
    };

    // review #23：dl_open 成功后复核 inode。若校验通过后、加载完成前文件
    // 被替换（换成另一个 inode），拒绝加载并立即关闭刚取得的句柄。
    if is_new {
        match posix::stat_path(&real, true) {
            Some(st) if st.dev == dev0 && st.ino == ino0 => {}
            _ => {
                let _ = posix::dl_close(handle);
                return Err(format!("{}: plugin file changed while loading", path));
            }
        }
    }

    let init_addr = match posix::dl_sym(handle, "wash_plugin_init") {
        Ok(a) if a != 0 => a,
        Ok(_) => {
            if is_new {
                let _ = posix::dl_close(handle);
            }
            return Err(format!("{}: no wash_plugin_init symbol", path));
        }
        Err(e) => {
            if is_new {
                let _ = posix::dl_close(handle);
            }
            return Err(e);
        }
    };

    // 唯一入口点：先调用 init 拿到 WashPluginInfo，随后立即做 ABI 协商，
    // 校验通过前不触碰 builtins 数组、不调用任何插件函数。
    let init: PluginInitFn = unsafe { std::mem::transmute::<usize, PluginInitFn>(init_addr) };
    let mut info = WashPluginInfo::zeroed();
    let rc = unsafe { init(&HOST_API, ABI_VERSION, &mut info) };
    if rc != 0 {
        if is_new {
            let _ = posix::dl_close(handle);
        }
        return Err(format!("{}: wash_plugin_init failed ({})", path, rc));
    }
    if info.abi_version != ABI_VERSION {
        if is_new {
            let _ = posix::dl_close(handle);
        }
        return Err(format!(
            "{}: ABI version mismatch (plugin {}, host {})",
            path, info.abi_version, ABI_VERSION
        ));
    }
    if info.struct_size != std::mem::size_of::<WashPluginInfo>() as u32 {
        if is_new {
            let _ = posix::dl_close(handle);
        }
        return Err(format!("{}: WashPluginInfo struct_size mismatch", path));
    }
    if info.builtins.is_null() || info.count == 0 {
        if is_new {
            let _ = posix::dl_close(handle);
        }
        return Err(format!("{}: plugin declares no builtins", path));
    }
    let count = info.count as usize;
    if count > 4096 {
        if is_new {
            let _ = posix::dl_close(handle);
        }
        return Err(format!("{}: unreasonable builtin count", path));
    }

    // 立即把 .so 数据段里的所有 CStr / 函数地址拷进 Rust 自有内存。
    let descs: &[WashBuiltinDesc] =
        unsafe { std::slice::from_raw_parts(info.builtins, count) };
    let on_unload = info.on_unload.map(|f| f as usize).unwrap_or(0);
    let mut new_items: Vec<DynamicBuiltin> = Vec::with_capacity(count);
    for d in descs {
        let name = match unsafe { cstr_copy(d.name) } {
            Some(n) if !n.is_empty() && !n.contains('/') => n,
            _ => {
                if is_new {
                    let _ = posix::dl_close(handle);
                }
                return Err(format!("{}: invalid builtin name", path));
            }
        };
        if crate::builtins::BUILTINS.contains(&name.as_str()) {
            if is_new {
                let _ = posix::dl_close(handle);
            }
            return Err(format!(
                "{}: builtin `{}' conflicts with a shell builtin",
                path, name
            ));
        }
        let fn_addr = d.fn_ptr.map(|f| f as usize).unwrap_or(0);
        if fn_addr == 0 {
            if is_new {
                let _ = posix::dl_close(handle);
            }
            return Err(format!("{}: builtin `{}' has a NULL function", path, name));
        }
        new_items.push(DynamicBuiltin {
            name,
            fn_addr,
            handle,
            plugin_path: real.clone(),
            help: unsafe { cstr_copy(d.help) },
            short_doc: unsafe { cstr_copy(d.short_doc) },
            on_unload,
        });
    }
    // 此后不再引用 info / descs 指向的 .so 数据段。

    // 与已有动态内建同名、且来自不同 .so 的：先卸载（可能触发旧 .so 的
    // on_unload + dlclose）；同一 .so 复用句柄时直接覆盖，绝不误关句柄。
    for it in &new_items {
        if let Some(old) = sh.plugins.dynamic.get(&it.name) {
            if old.plugin_path != real {
                let _ = unload_plugin(&it.name, sh);
            }
        }
    }

    for it in new_items {
        sh.plugins.dynamic.insert(it.name.clone(), it);
    }
    if is_new {
        sh.plugins.handles.insert(real.clone(), handle);
    }
    sh.plugins.whitelist = resolve_whitelist(sh);
    sync_global(&sh.plugins);
    Ok(())
}

/// 按名字卸载单个动态内建；同 .so 其余内建保留。仅当该 .so 的最后一个
/// 内建被注销后才调 on_unload + dlclose（函数指针失效前绝不 dlclose）。
pub fn unload_plugin(name: &str, sh: &mut Shell) -> Result<(), String> {
    let db = match sh.plugins.dynamic.remove(name) {
        Some(d) => d,
        None => return Err(format!("{}: not a shell builtin", name)),
    };
    let path = db.plugin_path.clone();
    let still_used = sh
        .plugins
        .dynamic
        .values()
        .any(|d| d.plugin_path == path);
    if !still_used {
        if db.on_unload != 0 {
            let f: OnUnloadFn =
                unsafe { std::mem::transmute::<usize, OnUnloadFn>(db.on_unload) };
            unsafe { f() };
        }
        let _ = posix::dl_close(db.handle);
        sh.plugins.handles.remove(&path);
    }
    sync_global(&sh.plugins);
    Ok(())
}

/// 调用一个动态内建：构造 C argv、栈上 ctx，transmute 函数地址并调用。
pub fn invoke(db: &DynamicBuiltin, argv: &[String], sh: &mut Shell) -> i32 {
    if db.fn_addr == 0 {
        return 1;
    }
    let cargs: Vec<CString> = argv
        .iter()
        .map(|s| CString::new(s.as_str()).unwrap_or_default())
        .collect();
    let mut ptrs: Vec<*mut c_char> = cargs
        .iter()
        .map(|c| c.as_ptr() as *mut c_char)
        .collect();
    ptrs.push(std::ptr::null_mut());
    let mut ctx = WashPluginCtx {
        shell: sh as *mut Shell as *mut c_void,
        api: &HOST_API,
        tmp: [0; 4096],
    };
    let f: WashBuiltinFn =
        unsafe { std::mem::transmute::<usize, WashBuiltinFn>(db.fn_addr) };
    unsafe { f(cargs.len() as c_int, ptrs.as_mut_ptr(), &mut ctx) }
}
