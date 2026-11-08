/*
 * tests/fixtures/plugin_hello.c -- wash C ABI example plugin (v1).
 *
 * Registers FOUR builtins in a single shared library, exercising every
 * WashPluginApi vtable slot (print / eprint / get_var / set_var / is_enabled)
 * and the hot-unload contract:
 *
 *   plhello [RC]     print "HELLO_FROM_PLUGIN\n" to stdout, return RC (default 0)
 *   plset NAME VALUE set a shell variable (no output)
 *   plget NAME       print the variable value, or "PLGET_UNSET:<name>\n"
 *   plecho TEXT      print TEXT to stderr via eprint
 *
 * This is wash's OWN ABI (include/wash_plugin.h), NOT bash's loadable-builtin
 * ABI.  See docs/spike-dlopen.md and the plan's P3 section.
 *
 * All descriptors / strings use static storage; they must stay alive for the
 * whole time the library is loaded.  The `api` pointer is host-owned and valid
 * for the whole load, so we keep it in static storage (allowed by the header).
 * The per-call `ctx` is NEVER cached.
 */
#include "wash_plugin.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

/* Host-owned vtable, valid for the entire load.  Set once in wash_plugin_init. */
static const WashPluginApi *g_api;

/*
 * plhello [RC]
 *   Always prints the fixed greeting to stdout through api->print.
 *   An optional first argument is parsed as the return code (exit-code
 *   pass-through).  We also call api->is_enabled once so that EVERY vtable
 *   slot is touched by this example plugin; the result is deliberately not
 *   allowed to change the output.
 */
static int bi_plhello(int argc, char **argv, WashPluginCtx *ctx)
{
    int rc_exit = 0;

    (void)g_api->is_enabled(ctx, "plhello");
    (void)g_api->print(ctx, "HELLO_FROM_PLUGIN\n");

    if (argc > 1 && argv[1] != NULL) {
        rc_exit = atoi(argv[1]);
    }
    return rc_exit;
}

/* plset NAME VALUE -- set a shell variable, produce no stdout on success. */
static int bi_plset(int argc, char **argv, WashPluginCtx *ctx)
{
    if (argc < 3 || argv[1] == NULL || argv[2] == NULL) {
        (void)g_api->eprint(ctx, "PLSET: need NAME VALUE\n");
        return 2;
    }
    (void)g_api->set_var(ctx, argv[1], argv[2]);
    return 0;
}

/*
 * plget NAME -- read a shell variable and print it.
 *   On success get_var returns 0 and *out points into the per-call scratch
 *   buffer inside ctx (valid only until the next api call), so we print it
 *   immediately.  When unset, emit an explicit, deterministic diagnostic.
 */
static int bi_plget(int argc, char **argv, WashPluginCtx *ctx)
{
    const char *name = (argc > 1 && argv[1] != NULL) ? argv[1] : "";
    const char *val = NULL;
    char buf[256];
    int n;

    if (g_api->get_var(ctx, name, &val) == 0 && val != NULL) {
        (void)g_api->print(ctx, val);
        (void)g_api->print(ctx, "\n");
    } else {
        n = snprintf(buf, sizeof(buf), "PLGET_UNSET:%s\n", name);
        if (n > 0) {
            (void)g_api->print(ctx, buf);
        }
    }
    return 0;
}

/* plecho TEXT -- write TEXT (plus newline) to stderr via api->eprint. */
static int bi_plecho(int argc, char **argv, WashPluginCtx *ctx)
{
    const char *text = (argc > 1 && argv[1] != NULL) ? argv[1] : "";
    (void)g_api->eprint(ctx, text);
    (void)g_api->eprint(ctx, "\n");
    return 0;
}

/* Static descriptor table: valid for the whole load. */
static const WashBuiltinDesc descs[] = {
    { "plhello", bi_plhello,
      "plhello -- print from plugin", "print a greeting (optional RC arg)" },
    { "plset", bi_plset,
      "plset -- set a shell variable", "plset NAME VALUE" },
    { "plget", bi_plget,
      "plget -- get a shell variable", "plget NAME" },
    { "plecho", bi_plecho,
      "plecho -- echo to stderr", "plecho TEXT" },
};

/* Called by the host after the last builtin from this .so is deregistered.
 * Kept as a real (no-op) function so the on_unload wiring is exercised. */
static void plugin_hello_unload(void)
{
    /* nothing to release */
}

/*
 * The one and only exported symbol.  Validate the host ABI / vtable before
 * filling `out`, exactly as the header's HARD CONTRACT requires.
 */
int wash_plugin_init(const WashPluginApi *api, uint32_t host_abi,
                     WashPluginInfo *out)
{
    if (host_abi != WASH_PLUGIN_ABI_VERSION) {
        return 1;
    }
    if (api == NULL || out == NULL) {
        return 2;
    }
    if (api->abi_version != WASH_PLUGIN_ABI_VERSION) {
        return 3;
    }
    if (api->struct_size < sizeof(WashPluginApi)) {
        return 4;
    }

    g_api = api;

    out->abi_version  = WASH_PLUGIN_ABI_VERSION;
    out->struct_size  = (uint32_t)sizeof(WashPluginInfo);
    out->flags        = 0;
    out->builtins     = descs;
    out->count        = 4u;
    out->plugin_name  = "plugin_hello";
    out->on_unload    = plugin_hello_unload;
    return 0;
}
