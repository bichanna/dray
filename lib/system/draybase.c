/* draybase.c - the hand-written half of the Dray runtime.
 * Compiled once per program, alongside the generated translation units. */
#include "draybase.h"

#include <stdio.h>
#include <stdlib.h>

DrayI64 dray_rc_live_count = 0;

void *dray_rc_alloc(DraySize payload, DrayDropFn drop) {
  DrayRcHeader *h = (DrayRcHeader *)calloc(1, sizeof(DrayRcHeader) + payload);
  if (!h)
    return NULL;
  h->strong = 1;
  h->weak = 0;
  h->drop = drop;
  dray_rc_live_count++;
  return (void *)(h + 1);
}

void dray_rc_retain(void *p) {
  if (!p)
    return;
  ((DrayRcHeader *)p - 1)->strong++;
}

void dray_rc_release(void *p) {
  if (!p)
    return;
  DrayRcHeader *h = (DrayRcHeader *)p - 1;
  if (--h->strong == 0) {
    if (h->drop)
      h->drop(p); /* release owned @T fields first */
    dray_rc_live_count--;
    if (h->weak == 0)
      free(h);
  }
}

DrayI64 dray_rc_live(void) { return dray_rc_live_count; }

void *dray_rc_downgrade(void *p) {
  if (!p)
    return NULL;
  ((DrayRcHeader *)p - 1)->weak++;
  return p;
}

void dray_rc_weak_release(void *p) {
  if (!p)
    return;

  DrayRcHeader *h = (DrayRcHeader *)p - 1;
  if (--h->weak == 0 && h->strong == 0)
    free(h);
}

void *dray_rc_upgrade(void *p) {
  if (!p)
    return NULL;

  DrayRcHeader *h = (DrayRcHeader *)p - 1;
  if (h->strong == 0)
    return NULL;

  h->strong++;
  return p;
}

void dray_index_fail(DrayI64 index, DrayI64 len) {
  fprintf(stderr, "dray: index %lld is out of bounds for length %lld\n",
          (long long)index, (long long)len);
  abort();
}

void dray_range_fail(DrayI64 lo, DrayI64 hi, DrayI64 len) {
  fprintf(stderr,
          "dray: slice range [%lld:%lld] is out of bounds for length %lld\n",
          (long long)lo, (long long)hi, (long long)len);
  abort();
}

void dray_range_from_fail(DrayI64 lo, DrayI64 len) {
  fprintf(stderr,
          "dray: slice range [%lld:] is out of bounds for length %lld\n",
          (long long)lo, (long long)len);
  abort();
}

DrayI64 dray_check_index(DrayI64 index, DrayI64 len) {
  if (index < 0 || index >= len)
    dray_index_fail(index, len);
  return index;
}

void dray_unreachable(void) {
  fprintf(stderr, "dray: reached a branch the compiler proved unreachable\n");
  abort();
}
