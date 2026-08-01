#include "draybase.h"

#include <stdio.h>
#include <stdlib.h>

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

void dray_alloc_fail(DraySize bytes) {
  fprintf(stderr, "dray: out of memory allocating %llu bytes\n",
          (unsigned long long)bytes);
  abort();
}

void dray_unreachable(void) {
  fprintf(stderr, "dray: reached a branch the compiler proved unreachable\n");
  abort();
}
