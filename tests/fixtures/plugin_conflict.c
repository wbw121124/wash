/*
 * tests/fixtures/plugin_conflict.c -- name collision with a STATIC builtin.
 *
 * Registers a builtin literally named "echo", which is a static wash/builtin
 * command.  The host MUST reject the library (a plugin may not shadow a
 * static builtin).  The contract suite additionally asserts that the static
 * `echo` is untouched: if the plugin were wrongly accepted, `echo PROBE`
 * would print CONFLICT_ECHO_MARKER instead of PROBE.
 *
 * Used by tests/plugin_contract.sh to prove static-name conflict rejection.
 */
#include "wash_plugin.h"

#include <stddef.h>  /* NULL */
#include <stdint.h>

static const WashPluginApi *g_api;

static int bi_conflict_echo(int argc, char **argv, WashPluginCtx *ctx)
{
    (void)argc;
    (void)argv;
    /* Distinctive marker: observable ONLY if the host wrongly accepts this
     * plugin and lets it replace the static `echo`. */
    (void)g_api->print(ctx, "CONFLICT_ECHO_MARKER\n");
    return 0;
}

static const WashBuiltinDesc descs[] = {
    { "echo", bi_conflict_echo,
      "echo -- conflicting builtin", "static-name collision probe" },
};

int wash_plugin_init(const WashPluginApi *api, uint32_t host_abi,
                     WashPluginInfo *out)
{
    if (host_abi != WASH_PLUGIN_ABI_VERSION) {
        return 1;
    }
    if (api == NULL || out == NULL) {
        return 2;
    }

    g_api = api;

    out->abi_version  = WASH_PLUGIN_ABI_VERSION;
    out->struct_size  = (uint32_t)sizeof(WashPluginInfo);
    out->flags        = 0;
    out->builtins     = descs;
    out->count        = 1u;
    out->plugin_name  = "plugin_conflict";
    out->on_unload    = NULL;
    return 0;
}
