/* draybase.h - the hand-written half of the Dray runtime.
 *
 * The base: the numeric typedefs, the compiler macros, and the small runtime
 * helpers (bounds checks, the unreachable marker). The reference-counting
 * runtime lives in its companion drayrc.h, which this file includes, so
 * generated code that includes draybase.h sees everything.
 *
 * Generated code never writes `int32_t` or `bool` directly. It writes `DrayI32`
 * and `DrayBool`, and this file decides what those mean. That indirection is
 * the point: a target where `stdint.h` is missing, or where a type needs a
 * different width, becomes a change to this file rather than to the code
 * generator.
 */

#ifndef DRAYBASE_H
#define DRAYBASE_H

/* ---------------------------------------------------------------- *
 *  Compiler and platform                                           *
 * ---------------------------------------------------------------- */

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

/* ---------------------------------------------------------------- *
 *  Numeric types                                                   *
 * ---------------------------------------------------------------- */

/* The usual case: a toolchain with fixed-width types. A target without them
 * defines DRAY_NO_STDINT and takes the fallback widths below. */
#ifndef DRAY_NO_STDINT
#include <stdint.h>
typedef int8_t DrayI8;
typedef int16_t DrayI16;
typedef int32_t DrayI32;
typedef int64_t DrayI64;
typedef uint8_t DrayU8;
typedef uint16_t DrayU16;
typedef uint32_t DrayU32;
typedef uint64_t DrayU64;
#else
typedef signed char DrayI8;
typedef short DrayI16;
typedef int DrayI32;
typedef long long DrayI64;
typedef unsigned char DrayU8;
typedef unsigned short DrayU16;
typedef unsigned int DrayU32;
typedef unsigned long long DrayU64;
#endif

typedef float DrayF32;
typedef double DrayF64;

/* Dray's `cchar` is C's `char`, which the standard keeps distinct from both
 * `signed char` and `unsigned char`. It exists only so an `extern` can match a
 * real C signature. */
typedef char DrayChar;

#include <stddef.h>
typedef size_t DraySize;
typedef ptrdiff_t DrayISize;

#include <stdbool.h>
typedef bool DrayBool;

/* ---------------------------------------------------------------- *
 *  Reference counting                                              *
 * ---------------------------------------------------------------- */

/* The reference-counting runtime is its own file, kept separate so the base
 * above stays small. It needs the typedefs declared here, so it is included
 * after them, not before. */
#include "drayrc.h"

/* Both print to stderr and abort. They are the only way a Dray program stops
 * on a bad index, so they say which index and which length, not just that
 * something went wrong. */
DRAY_NORETURN void dray_index_fail(DrayI64 index, DrayI64 len);
DRAY_NORETURN void dray_range_fail(DrayI64 lo, DrayI64 hi, DrayI64 len);
DRAY_NORETURN void dray_range_from_fail(DrayI64 lo, DrayI64 len);

/* Returns `index` when it is in range. Used where the length is a compile-time
 * constant, so the generated C reads `arr[dray_check_index(i, 4)]`. */
DrayI64 dray_check_index(DrayI64 index, DrayI64 len);

/* Closes a proc whose every path returns in a way C cannot see—an exhaustive
 * `switch`, say. Dray has already proved this is unreachable. Saying so keeps
 * `-Werror=return-type` quiet without inventing a return value. */
DRAY_NORETURN void dray_unreachable(void);

#endif /* DRAYBASE_H */
