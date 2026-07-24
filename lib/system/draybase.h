/* draybase.h - the hand-written half of the Dray runtime.
 *
 * Everything the generated C needs that is not worth generating: the numeric
 * typedefs, the reference-counting header, and the handful of macros whose
 * spelling differs between compilers.
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

typedef void (*DrayDropFn)(void *);

typedef struct {
  DrayU32 strong;
  DrayU32 weak;
  DrayDropFn drop;
} DrayRcHeader;

/* The header sits immediately before the payload, so a `@T` value is an
 * ordinary `T *` as far as C is concerned. */
extern DrayI64 dray_rc_live_count;

void *dray_rc_alloc(DraySize payload, DrayDropFn drop);
void dray_rc_retain(void *p);
void dray_rc_release(void *p);
DrayI64 dray_rc_live(void);

#endif /* DRAYBASE_H */
