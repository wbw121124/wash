/*
 * tests/fixtures/plugin_badabi.c -- deliberately WRONG ABI version.
 *
 * Registers one (never-usable) builtin "plbadabi" and reports
 * WashPluginInfo.abi_version = 999.  The host MUST reject the library before
 * invoking any builtin: it validates info.abi_version (and info.struct_size)
 * right after wash_plugin_init returns, then dlcloses the library.
 *
 * Used by tests/plugin_contract.sh to prove ABI-version gating.
 */
#include "wash_plugin.h"

#include <stddef.h>  /* NULL */
#include <stdint.h>

static int bi_noop(int argc, char **argv, WashPluginCtx *ctx)
{
    (void)argc;
    (void)argv;
    (void)ctx;
    return 0;
}

static const WashBuiltinDesc descs[] = {
    { "plbadabi", bi_noop,
      "plbadabi -- deliberately wrong ABI", "ABI rejection probe" },
};

int wash_plugin_init(const WashPluginApi *api, uint32_t host_abi,
                     WashPluginInfo *out)
{
    (void)api;
    (void)host_abi;
    if (out == NULL) {
        return 2;
    }

    /* The single point of this fixture: an ABI version the host cannot know. */
    out->abi_version  = 999u;
    out->struct_size  = (uint32_t)sizeof(WashPluginInfo);
    out->flags        = 0;
    out->builtins     = descs;
    out->count        = 1u;
    out->plugin_name  = "plugin_badabi";
    out->on_unload    = NULL;
    return 0;
}
