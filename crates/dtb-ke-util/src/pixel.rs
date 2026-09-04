//! In-place R↔B channel swap for tightly packed 32-bit pixels (`RGBA ⇄ BGRA`).
//!
//! `typst-render` produces RGBA; gpui stores images as BGRA. The conversion is
//! a hot memory pass (~8 MB per A4 page at 2×), so it gets SIMD fast paths:
//! NEON on AArch64, AVX2 then SSSE3 on x86/x86-64, and a scalar fallback that
//! also mops up the tail. The operation is its own inverse.

/// Byte-shuffle index that turns `[r,g,b,a]` into `[b,g,r,a]`, four pixels wide.
#[cfg(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64"))]
const SHUF16: [u8; 16] = [2, 1, 0, 3, 6, 5, 4, 7, 10, 9, 8, 11, 14, 13, 12, 15];

/// Swap byte 0 and byte 2 of every 4-byte pixel in `buf`, in place.
///
/// `buf.len()` is expected to be a multiple of 4; a trailing partial pixel is
/// left untouched.
pub fn swap_rb(buf: &mut [u8]) {
    #[cfg(target_arch = "aarch64")]
    {
        swap_rb_neon(buf);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: guarded by the runtime feature check.
            unsafe { swap_rb_avx2(buf) };
        } else if std::is_x86_feature_detected!("ssse3") {
            // SAFETY: guarded by the runtime feature check.
            unsafe { swap_rb_ssse3(buf) };
        } else {
            swap_rb_scalar(buf);
        }
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64")))]
    {
        swap_rb_scalar(buf);
    }
}

/// The portable path. Also handles the ragged tail left by the SIMD paths.
pub fn swap_rb_scalar(buf: &mut [u8]) {
    let (pixels, _tail) = buf.as_chunks_mut::<4>();
    for px in pixels {
        px.swap(0, 2);
    }
}

#[cfg(target_arch = "aarch64")]
fn swap_rb_neon(buf: &mut [u8]) {
    use core::arch::aarch64::{vld1q_u8, vqtbl1q_u8, vst1q_u8};

    let (chunks, tail) = buf.as_chunks_mut::<16>();
    // SAFETY: NEON is mandatory on AArch64; each `chunk` is exactly 16 bytes,
    // so the unaligned 128-bit loads/stores are in bounds.
    unsafe {
        let idx = vld1q_u8(SHUF16.as_ptr());
        for chunk in chunks {
            let v = vqtbl1q_u8(vld1q_u8(chunk.as_ptr()), idx);
            vst1q_u8(chunk.as_mut_ptr(), v);
        }
    }
    swap_rb_scalar(tail);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn swap_rb_avx2(buf: &mut [u8]) {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::{
        _mm256_loadu_si256, _mm256_setr_epi8, _mm256_shuffle_epi8, _mm256_storeu_si256,
    };
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::{
        _mm256_loadu_si256, _mm256_setr_epi8, _mm256_shuffle_epi8, _mm256_storeu_si256,
    };

    // `_mm256_shuffle_epi8` shuffles within each 128-bit lane, so the mask is
    // the 16-byte pattern repeated for both halves.
    let s = &SHUF16;
    // SAFETY: the caller has verified AVX2; loads/stores are exactly 32 bytes.
    unsafe {
        #[rustfmt::skip]
        let mask = _mm256_setr_epi8(
            s[0] as i8, s[1] as i8, s[2] as i8, s[3] as i8,
            s[4] as i8, s[5] as i8, s[6] as i8, s[7] as i8,
            s[8] as i8, s[9] as i8, s[10] as i8, s[11] as i8,
            s[12] as i8, s[13] as i8, s[14] as i8, s[15] as i8,
            s[0] as i8, s[1] as i8, s[2] as i8, s[3] as i8,
            s[4] as i8, s[5] as i8, s[6] as i8, s[7] as i8,
            s[8] as i8, s[9] as i8, s[10] as i8, s[11] as i8,
            s[12] as i8, s[13] as i8, s[14] as i8, s[15] as i8,
        );
        let (chunks, tail) = buf.as_chunks_mut::<32>();
        for chunk in chunks {
            let v = _mm256_loadu_si256(chunk.as_ptr().cast());
            let v = _mm256_shuffle_epi8(v, mask);
            _mm256_storeu_si256(chunk.as_mut_ptr().cast(), v);
        }
        swap_rb_scalar(tail);
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "ssse3")]
unsafe fn swap_rb_ssse3(buf: &mut [u8]) {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::{_mm_loadu_si128, _mm_shuffle_epi8, _mm_storeu_si128};
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::{_mm_loadu_si128, _mm_shuffle_epi8, _mm_storeu_si128};

    // SAFETY: the caller has verified SSSE3; loads/stores are exactly 16 bytes.
    unsafe {
        let mask = _mm_loadu_si128(SHUF16.as_ptr().cast());
        let (chunks, tail) = buf.as_chunks_mut::<16>();
        for chunk in chunks {
            let v = _mm_loadu_si128(chunk.as_ptr().cast());
            let v = _mm_shuffle_epi8(v, mask);
            _mm_storeu_si128(chunk.as_mut_ptr().cast(), v);
        }
        swap_rb_scalar(tail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(buf: &[u8]) -> Vec<u8> {
        let mut out = buf.to_vec();
        for px in out.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        out
    }

    #[test]
    fn one_pixel() {
        let mut px = [10u8, 20, 30, 40];
        swap_rb(&mut px);
        assert_eq!(px, [30, 20, 10, 40]);
    }

    #[test]
    fn simd_matches_scalar_across_lengths_and_tails() {
        for len in [
            0usize, 4, 8, 12, 16, 20, 28, 32, 33, 36, 60, 64, 128, 129, 131, 1024, 4093, 8192, 8193,
        ] {
            let orig: Vec<u8> = (0..len).map(|i| (i.wrapping_mul(37) % 251) as u8).collect();

            let mut got = orig.clone();
            swap_rb(&mut got);
            assert_eq!(got, reference(&orig), "len {len}");

            // Its own inverse (tail bytes were never touched).
            swap_rb(&mut got);
            assert_eq!(got, orig, "involution, len {len}");
        }
    }

    #[test]
    fn scalar_path_directly() {
        let orig: Vec<u8> = (0..2000u32).map(|i| (i % 251) as u8).collect();
        let mut a = orig.clone();
        swap_rb_scalar(&mut a);
        assert_eq!(a, reference(&orig));
    }
}
