/* draybase.c - the hand-written half of the Dray runtime.
 * Compiled once per program, alongside the generated translation units. */
#include "draybase.h"

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
