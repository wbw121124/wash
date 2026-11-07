/*
 * wash_plugin.h -- stable C ABI for wash binary plugins (v1).
 *
 * A wash plugin is an ordinary shared library (MSYS2: .so or .dll; Linux: .so)
 * loaded at runtime with `enable -f FILE [NAME ...]`.  It exports exactly one
 * symbol, wash_plugin_init().  The host does NOT export its own symbols back
 * to the plugin (Cygwin cannot resolve a DLL's references into the host
 * executable -- see docs/spike-dlopen.md), so every service the plugin needs
 * is handed to it through the WashPluginApi vtable below.  No -rdynamic /
 * --export-dynamic is required on the host.
 *
 * ABI direction: the PLUGIN exports, the HOST feeds a vtable.  All strings
 * and the builtins[] array live in the plugin's own data segment.
 *
 * HARD CONTRACT (violating any of these is undefined behaviour):
 *   - WashPluginCtx is opaque and strictly PER-CALL.  A plugin MUST NOT cache
 *     the ctx pointer, keep it past the current builtin invocation, or share
 *     it across threads.  The host rebuilds ctx on the stack for every call.
 *   - The WashPluginApi table and every function pointer inside it are owned
 *     by the host and stay valid for the ENTIRE time the library is loaded.
 *     A plugin MAY keep the `api` pointer (or copy the function pointers) in
 *     its own static storage and call them from its builtins, passing the
 *     per-call ctx it received.  There is no need to copy them per call.
 *   - The C string returned through WashPluginApi.get_var's `out` is valid
 *     only until the NEXT call into the WashPluginApi (the host copies the
 *     value into a per-call scratch buffer inside ctx).  Copy it out at once
 *     if you need to retain it.
 *   - name / help / short_doc / plugin_name and the builtins[] array MUST
 *     stay valid for the whole time the library is loaded (use static
 *     storage).  The host copies every string into its own memory at load time
 *     and keeps no pointer into the .so, but it does read them during
 *     wash_plugin_init and before dlclose.
 *   - Function pointers (WashBuiltinDesc.fn, WashPluginInfo.on_unload) are
 *     cached by the host as raw addresses.  The host guarantees it never calls
 *     a function pointer after the library was unloaded: unload order is
 *     deregister -> on_unload -> dlclose, and dlclose only happens after the
 *     last builtin from that .so is deregistered.
 *
 * The host reserves the right to reject a plugin (and dlclose it immediately)
 * BEFORE calling any of its functions when the ABI does not match: host_abi,
 * WashPluginApi.abi_version / struct_size and WashPluginInfo.abi_version /
 * struct_size are all checked first.
 *
 * NOTE: this is wash's own ABI, NOT GNU bash's loadable-builtin ABI.  A .so
 * built against bash's builtins.h cannot be loaded by wash and vice versa.
 */
#ifndef WASH_PLUGIN_H
#define WASH_PLUGIN_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bump together with the Rust-side ABI_VERSION in src/plugin.rs. */
#define WASH_PLUGIN_ABI_VERSION 1u
/* Reserved flag: request an isolated crash domain.  Not implemented in v1;
 * plugins should leave flags = 0.  Kept so the ABI can grow without a break. */
#define WASH_PLUGIN_ISOLATE 0x1u

/* Opaque, host-private invocation context.  Never dereference it. */
typedef struct WashPluginCtx WashPluginCtx;

/*
 * Services the host provides to the plugin.  The host owns this table, fills
 * abi_version / struct_size and passes a pointer to wash_plugin_init().
 * Return conventions: print / eprint / get_var / set_var return 0 on success
 * and non-zero on error; is_enabled returns 1 for "enabled" and 0 for
 * "disabled / unknown".
 */
typedef struct WashPluginApi {
    uint32_t abi_version;   /* = WASH_PLUGIN_ABI_VERSION */
    uint32_t struct_size;   /* = sizeof(WashPluginApi) */
    void    *reserved;      /* NULL; reserved for future use */
    int (*print)(WashPluginCtx *ctx, const char *text);   /* write to stdout */
    int (*eprint)(WashPluginCtx *ctx, const char *text);  /* write to stderr */
    int (*get_var)(WashPluginCtx *ctx, const char *name, const char **out);
    int (*set_var)(WashPluginCtx *ctx, const char *name, const char *value);
    int (*is_enabled)(WashPluginCtx *ctx, const char *name);
} WashPluginApi;

/* Signature of every builtin a plugin registers.  argv is a conventional
 * NULL-terminated vector of `argc` entries; ctx is valid for this call only.
 * The return value becomes the builtin's exit status. */
typedef int (*WashBuiltinFn)(int argc, char **argv, WashPluginCtx *ctx);

typedef struct WashBuiltinDesc {
    const char    *name;       /* builtin name, e.g. "plhello" */
    WashBuiltinFn  fn;         /* implementation */
    const char    *help;       /* usage synopsis shown by `help NAME` */
    const char    *short_doc;  /* one-line description shown by `help NAME` */
} WashBuiltinDesc;

typedef struct WashPluginInfo {
    uint32_t abi_version;              /* = WASH_PLUGIN_ABI_VERSION */
    uint32_t struct_size;              /* = sizeof(WashPluginInfo) */
    uint32_t flags;                    /* 0 or WASH_PLUGIN_ISOLATE (reserved) */
    const WashBuiltinDesc *builtins;   /* array of `count` entries */
    uint32_t count;
    const char *plugin_name;
    void (*on_unload)(void);           /* optional, may be NULL */
} WashPluginInfo;

/*
 * The one and only symbol a wash plugin must export.
 *   api      : host-owned vtable (valid for the whole load, see above).
 *   host_abi : ABI version the host implements (= WASH_PLUGIN_ABI_VERSION).
 *   out      : the plugin fills this with its descriptors.
 * Returns 0 on success, non-zero on failure (the host will dlclose the lib).
 */
int wash_plugin_init(const WashPluginApi *api, uint32_t host_abi,
                     WashPluginInfo *out);

#ifdef __cplusplus
}
#endif

#endif /* WASH_PLUGIN_H */
