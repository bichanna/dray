// SPDX-License-Identifier: Apache-2.0

//! IR → C lowering, via Tamago.

use dray_ir::Ir;

mod lower;

pub use lower::lower_ir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenError {
    pub message: String,
}

impl CodegenError {
    pub(crate) fn new(message: impl Into<String>) -> CodegenError {
        CodegenError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "codegen: {}", self.message)
    }
}

impl std::error::Error for CodegenError {}

pub type Result<T> = std::result::Result<T, CodegenError>;

/// The reference-counting runtime, emitted ahead of the program only when the IR
/// actually uses RC. The header sits immediately before each payload so a bare
/// `@T` value is an ordinary `T*` from C's point of view. `calloc`
/// zero-initializes the payload, giving `alloc T` its zero value.
const RC_RUNTIME: &str = "\
#include <stdint.h>
#include <stdlib.h>

typedef struct { uint32_t strong; uint32_t weak; } DrayRcHeader;

int64_t dray_rc_live_count = 0;

void *dray_rc_alloc(unsigned long payload) {
    DrayRcHeader *h = (DrayRcHeader *)calloc(1, sizeof(DrayRcHeader) + payload);
    h->strong = 1;
    h->weak = 0;
    dray_rc_live_count++;
    return (void *)(h + 1);
}

void dray_rc_retain(void *p) {
    if (!p) return;
    ((DrayRcHeader *)p - 1)->strong++;
}

void dray_rc_release(void *p) {
    if (!p) return;
    DrayRcHeader *h = (DrayRcHeader *)p - 1;
    if (--h->strong == 0) {
        dray_rc_live_count--;
        if (h->weak == 0) free(h);
    }
}

int64_t dray_rc_live(void) { return dray_rc_live_count; }
";

/// Lower a whole IR module to C source.
pub fn ir_to_c(ir: &Ir) -> Result<String> {
    let scope = lower_ir(ir)?;
    let program = format!("{scope}");
    if ir.uses_rc {
        Ok(format!("{RC_RUNTIME}\n{program}"))
    } else {
        Ok(program)
    }
}
