#include "drayrc.h"

#include <stdlib.h>

DrayI64 dray_rc_live_count = 0;

void *dray_rc_alloc(DraySize payload, DrayDropFn drop) {
  void *p = dray_rc_try_alloc(payload, drop);
  if (!p)
    dray_alloc_fail(sizeof(DrayRcHeader) + payload);
  return p;
}

void *dray_rc_try_alloc(DraySize payload, DrayDropFn drop) {
  DrayRcHeader *h = (DrayRcHeader *)calloc(1, sizeof(DrayRcHeader) + payload);
  if (!h)
    return NULL;
  h->strong = 1;
  h->weak = 0;
  h->count = 1;
  h->drop = drop;
  dray_rc_live_count++;
  return (void *)(h + 1);
}

void *dray_rc_alloc_array(DraySize count, DraySize stride, DrayDropFn drop) {
  void *p = dray_rc_alloc(count * stride, drop);
  if (p)
    ((DrayRcHeader *)p - 1)->count = count;
  return p;
}

DraySize dray_rc_count(void *p) { return ((DrayRcHeader *)p - 1)->count; }

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
