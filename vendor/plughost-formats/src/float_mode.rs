//! The floating-point mode of a plugin's audio calls. Plugins process with subnormal numbers
//! flushed to zero, as inputs and as results, as audio hosts commonly run them: denormal
//! arithmetic is slow on x86-64, and the same mode on every platform and in every processing path
//! keeps their output identical. The mode holds only during the call, including the host
//! callbacks the plugin makes from it; plughost's own code runs in the default mode Rust assumes.

/// Flushes subnormal inputs and results to zero until it drops, then restores the mode it found.
#[must_use]
pub(crate) struct FlushToZero {
    previous: platform::Register,
}

impl FlushToZero {
    #[inline]
    pub(crate) fn enter() -> FlushToZero {
        let previous = platform::read();
        let flushing = previous | platform::FLUSH;
        if flushing != previous {
            platform::write(flushing);
        }
        FlushToZero { previous }
    }
}

impl Drop for FlushToZero {
    #[inline]
    fn drop(&mut self) {
        // Restored even when the plugin left the mode changed.
        if platform::read() != self.previous {
            platform::write(self.previous);
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod platform {
    use std::arch::asm;

    pub type Register = u32;
    /// MXCSR flush-to-zero (bit 15) and denormals-are-zero (bit 6).
    pub const FLUSH: Register = 0x8040;

    #[inline]
    pub fn read() -> Register {
        let mut value: Register = 0;
        // SAFETY: STMXCSR stores the 32-bit control register at the given address.
        unsafe { asm!("stmxcsr [{}]", in(reg) &mut value, options(nostack, preserves_flags)) };
        value
    }

    #[inline]
    pub fn write(value: Register) {
        // SAFETY: LDMXCSR loads a value read from the register with only flush bits changed.
        unsafe {
            asm!("ldmxcsr [{}]", in(reg) &value, options(nostack, preserves_flags, readonly))
        };
    }
}

#[cfg(target_arch = "aarch64")]
mod platform {
    use std::arch::asm;

    pub type Register = u64;
    /// FPCR.FZ (bit 24), which flushes subnormal inputs and results with FPCR.AH clear.
    pub const FLUSH: Register = 1 << 24;

    #[inline]
    pub fn read() -> Register {
        let value: Register;
        // SAFETY: reading FPCR has no side effects.
        unsafe { asm!("mrs {}, fpcr", out(reg) value, options(nomem, nostack, preserves_flags)) };
        value
    }

    #[inline]
    pub fn write(value: Register) {
        // SAFETY: the value was read from FPCR with only the flush bit changed.
        unsafe { asm!("msr fpcr, {}", in(reg) value, options(nomem, nostack, preserves_flags)) };
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("plughost supports x86-64 and AArch64");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_holds_only_while_the_guard_lives() {
        let before = platform::read();
        {
            let _flushing = FlushToZero::enter();
            assert_eq!(platform::read() & platform::FLUSH, platform::FLUSH);
        }
        assert_eq!(platform::read(), before);
    }
}
