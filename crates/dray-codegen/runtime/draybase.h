/* draybase.h - the hand-written half of the Dray runtime.
   Embedded in the compiler binary and written beside the generated C. */
#ifndef DRAYBASE_H
#define DRAYBASE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

/* Compilers disagree on how to spell these, and the disagreement is not
   interesting enough to leak into the code generator. */
#if defined(_MSC_VER)
#define DRAY_INLINE __inline
#define DRAY_NORETURN __declspec(noreturn)
#define DRAY_UNUSED
#elif defined(__GNUC__) || defined(__clang__)
#define DRAY_INLINE __inline__
#define DRAY_NORETURN __attribute__((noreturn))
#define DRAY_UNUSED __attribute__((unused))
#else
#define DRAY_INLINE
#define DRAY_NORETURN
#define DRAY_UNUSED
#endif

/* Every allocation carries a drop function: for an aggregate holding `@T`
   fields it releases them, so freeing a node frees what it owns, recursively.
   NULL when there is nothing to release. */
typedef void (*DrayDropFn)(void *);

typedef struct {
  uint32_t strong;
  uint32_t weak;
  DrayDropFn drop;
} DrayRcHeader;

/* The header sits immediately before the payload, so a `@T` value is an
   ordinary `T *` as far as C is concerned. */
extern int64_t dray_rc_live_count;

void *dray_rc_alloc(size_t payload, DrayDropFn drop);
void dray_rc_retain(void *p);
void dray_rc_release(void *p);
int64_t dray_rc_live(void);

#endif /* DRAYBASE_H */
