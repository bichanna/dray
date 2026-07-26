/* drayrc.h - Dray's reference-counting runtime.
 *
 * Split out of draybase so the base stays small: draybase.h is primitive types
 * and toolchain macros, and everything about heap objects and their counts
 * lives here. draybase.h includes this, so generated code that includes
 * draybase.h sees these declarations without naming this file.
 *
 * Layout: the header sits immediately before the payload, so a `@T` value is an
 * ordinary `T *` and `(DrayRcHeader *)p - 1` finds its header. */
#ifndef DRAYRC_H
#define DRAYRC_H

#include "draybase.h"

/* Run once when an object's last strong reference goes, to release the
 * references that object owns. NULL for objects that own none. */
typedef void (*DrayDropFn)(void *);

typedef struct {
  DrayU32 strong;
  DrayU32 weak;
  /* Element count. 1 for a scalar `@T`; the length for a `@[]T`, whose drop
   * function needs it to walk the payload. The type `@[]T` erases from, so the
   * count cannot come from anywhere else. */
  DraySize count;
  DrayDropFn drop;
} DrayRcHeader;

/* A running count of live heap objects, for leak checks in tests. */
extern DrayI64 dray_rc_live_count;

/* Allocate `payload` zeroed bytes with a fresh header (strong 1, weak 0). */
void *dray_rc_alloc(DraySize payload, DrayDropFn drop);

/* Allocate `count` elements of `stride` bytes each, zeroed, recording the count
 * in the header. `drop` runs once for the whole payload, not per element: an
 * array drop function loops using `dray_rc_count`. */
void *dray_rc_alloc_array(DraySize count, DraySize stride, DrayDropFn drop);

/* The element count recorded at allocation, for an array drop function. */
DraySize dray_rc_count(void *p);

void dray_rc_retain(void *p);
void dray_rc_release(void *p);
DrayI64 dray_rc_live(void);

/* Weak references. A weak reference does not keep the payload alive; it keeps
 * the *header* alive, so an upgrade can ask whether the payload is still there.
 * That is the two-phase free: the payload dies when the last strong reference
 * goes, the header when the last weak one does. */
void *dray_rc_downgrade(void *p);
void dray_rc_weak_release(void *p);

/* NULL when the payload is gone. On success the strong count is incremented, so
 * the caller receives an owning reference like any other. */
void *dray_rc_upgrade(void *p);

#endif /* DRAYRC_H */
