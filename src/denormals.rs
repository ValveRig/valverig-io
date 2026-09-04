//! Flushing denormal results to zero on the thread running the audio.
//!
//! Denormal arithmetic costs tens of times more per operation on some
//! processors. A reverb tail or a filter's ringing decaying towards zero
//! reaches that range, and it shows up as a callback that intermittently
//! overruns its budget rather than as anything audible.
//!
//! [`flush_to_zero`] sets the mode on the calling thread. The output
//! callback calls it on every block and never puts it back, because the
//! thread it runs on exists to run audio and ends with the stream.

/// Make this thread's arithmetic flush denormal results to zero.
///
/// Cheap enough to call on every callback. Does nothing on an architecture
/// with no such mode to reach.
#[cfg(target_arch = "aarch64")]
#[inline]
pub(crate) fn flush_to_zero() {
    /// FPCR bit 24, "flushing denormalized numbers to zero". On aarch64 it
    /// covers operands as well as results.
    const FZ: u64 = 1 << 24;

    // SAFETY: FPCR is this thread's own floating-point control register and
    // both instructions are unprivileged on aarch64. The value written back
    // differs from the one read only in FZ, so the rounding mode and every
    // other setting survive.
    unsafe {
        let fpcr: u64;
        std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
        std::arch::asm!("msr fpcr, {}", in(reg) fpcr | FZ, options(nomem, nostack, preserves_flags));
    }
}

/// Make this thread's arithmetic flush denormal results to zero, and treat
/// denormal operands as zero too.
///
/// Cheap enough to call on every callback. Does nothing on an architecture
/// with no such mode to reach.
#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "x86", target_feature = "sse")
))]
#[inline]
pub(crate) fn flush_to_zero() {
    /// MXCSR bit 15, flush-to-zero, and bit 6, denormals-are-zero.
    const FTZ_DAZ: u32 = (1 << 15) | (1 << 6);

    let mut found: u32 = 0;
    // SAFETY: MXCSR is this thread's own floating-point control register and
    // both instructions are unprivileged. `stmxcsr` writes the four bytes of
    // `found` and `ldmxcsr` reads four bytes back. The value written differs
    // from the one read only in FTZ and DAZ, so the rounding mode and the
    // exception masks survive.
    unsafe {
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut found, options(nostack, preserves_flags));
        let armed = found | FTZ_DAZ;
        std::arch::asm!("ldmxcsr [{}]", in(reg) &armed, options(nostack, preserves_flags, readonly));
    }
}

/// Nothing to do on an architecture with no flush-to-zero mode this crate
/// knows how to reach.
#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "x86_64",
    all(target_arch = "x86", target_feature = "sse")
)))]
#[inline]
pub(crate) fn flush_to_zero() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest denormal a multiply can land on, times one, is still
    /// denormal unless the result is flushed.
    fn denormal_survives() -> bool {
        let x = f32::from_bits(0x0000_0100);
        let product = std::hint::black_box(x) * std::hint::black_box(1.0f32);
        std::hint::black_box(product).to_bits() != 0
    }

    /// Armed on a thread of its own and left armed, the way the device
    /// callback leaves the thread it runs on. Nothing restores the mode, so
    /// the arming happens somewhere it cannot outlive.
    #[test]
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        all(target_arch = "x86", target_feature = "sse")
    ))]
    fn arming_flushes_denormal_results_on_the_thread_that_armed_it() {
        std::thread::spawn(|| {
            assert!(denormal_survives(), "the thread starts without flushing");
            flush_to_zero();
            assert!(
                !denormal_survives(),
                "armed, a denormal result becomes zero"
            );
        })
        .join()
        .expect("the arming thread panicked");

        assert!(
            denormal_survives(),
            "and the mode belongs to that thread alone"
        );
    }
}
