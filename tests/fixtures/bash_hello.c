/*
 * tests/fixtures/bash_hello.c -- GNU bash loadable builtins for the WSL
 * "second oracle" capability-equivalence probe
 * (tests/plugin_second_oracle.sh).
 *
 * This is GNU bash's loadable-builtin ABI (builtins.h / shell.h), NOT wash's
 * C ABI.  Real bash 5.3.0 loads this .so and calls hello/hset/hget/hecho,
 * while wash loads its OWN plugin_hello.so and calls plhello/plset/plget/
 * plecho.  Both sides must exhibit the SAME five capabilities: output,
 * exit-code pass-through, variable read/write, four builtins registered by
 * ONE `enable -f`, and per-name hot unload.
 *
 * This is a behavioural / capability comparison, NOT byte-level ABI
 * compatibility -- bash-ABI .so files can NEVER be loaded by wash and vice
 * versa.
 *
 * Verified build (all three -I flags are required on this WSL box):
 *   gcc -fPIC -shared \
 *     -I/usr/local/include/bash \
 *     -I/usr/local/include/bash/include \
 *     -I/usr/local/include/bash/builtins \
 *     -o bash_hello.so bash_hello.c
 */
#include <stdio.h>
#include <stdlib.h>   /* atoi */
#include <stddef.h>   /* NULL */

#include "builtins.h"
#include "shell.h"
#include "bashgetopt.h"
#include "common.h"
#include "variables.h"  /* bind_variable, get_string_value */

static char *word0(WORD_LIST *l)
{
    return (l && l->word) ? l->word->word : NULL;
}

static char *word1(WORD_LIST *l)
{
    return (l && l->next && l->next->word) ? l->next->word->word : NULL;
}

/* hello [RC] -- fixed greeting on stdout, optional exit-code pass-through. */
int hello_builtin(WORD_LIST *list)
{
    char *rc = word0(list);
    puts("HELLO_FROM_PLUGIN");
    return rc ? atoi(rc) : 0;
}

/* hset NAME VALUE -- write a shell variable through bash's own variable API. */
int hset_builtin(WORD_LIST *list)
{
    char *name = word0(list);
    char *value = word1(list);
    if (name == NULL || value == NULL) {
        fprintf(stderr, "HSET: need NAME VALUE\n");
        return 1;
    }
    bind_variable(name, value, 0);
    return 0;
}

/* hget NAME -- read it back; unset -> the same marker wash's plget emits. */
int hget_builtin(WORD_LIST *list)
{
    char *name = word0(list);
    char *v;
    if (name == NULL) {
        name = "";
    }
    v = get_string_value(name);
    if (v != NULL) {
        puts(v);
    } else {
        printf("PLGET_UNSET:%s\n", name);
    }
    return 0;
}

/* hecho TEXT -- write TEXT to stderr. */
int hecho_builtin(WORD_LIST *list)
{
    char *text = word0(list);
    fprintf(stderr, "%s\n", text ? text : "");
    return 0;
}

char *hello_doc[] = { "hello from plugin", NULL };
char *hset_doc[]  = { "hset -- set a shell variable", NULL };
char *hget_doc[]  = { "hget -- get a shell variable", NULL };
char *hecho_doc[] = { "hecho -- echo to stderr", NULL };

struct builtin hello_struct = {
    "hello", hello_builtin, BUILTIN_ENABLED, hello_doc, "hello", 0
};
struct builtin hset_struct = {
    "hset", hset_builtin, BUILTIN_ENABLED, hset_doc, "hset", 0
};
struct builtin hget_struct = {
    "hget", hget_builtin, BUILTIN_ENABLED, hget_doc, "hget", 0
};
struct builtin hecho_struct = {
    "hecho", hecho_builtin, BUILTIN_ENABLED, hecho_doc, "hecho", 0
};

/* bash calls <name>_builtin_load / <name>_builtin_unload when present. */
int hello_builtin_load(void) { return 1; }
void hello_builtin_unload(void) { }
int hset_builtin_load(void) { return 1; }
void hset_builtin_unload(void) { }
int hget_builtin_load(void) { return 1; }
void hget_builtin_unload(void) { }
int hecho_builtin_load(void) { return 1; }
void hecho_builtin_unload(void) { }
