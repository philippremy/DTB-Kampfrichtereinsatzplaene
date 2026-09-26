//! iOS symbolication with `dladdr`: what a process can learn about the OS libraries it has loaded, without
//! reading the dyld shared cache (which an app's sandbox does not allow).
//!
//! `dladdr` only knows **exported** symbols. For a private function it answers with the nearest exported symbol
//! *before* it — a wrong name with a large offset — so a hit farther than [`MAX_REACH`] from its symbol is
//! dropped, and everything this module produces is published as *approximate*.

use std::ffi::CStr;

use dtb_ke_crash::syshints::{Entry, FrameRef};

use crate::cache::{Uuid, display_name};

/// Beyond this distance from the symbol `dladdr` returned, the name is treated as unrelated.
pub const MAX_REACH: u64 = 0x1_0000;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_UUID: u32 = 0x1b;

unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_header(index: u32) -> *const u8;
}

/// A loaded image: its header address and UUID.
#[derive(Clone, Copy)]
pub struct Loaded {
    pub header: usize,
    pub uuid: Uuid,
}

/// The `LC_UUID` of the Mach-O image whose header is at `header`.
///
/// # Safety
/// `header` must point at a mapped 64-bit Mach-O header (as `_dyld_get_image_header` returns).
pub unsafe fn image_uuid(header: *const u8) -> Option<Uuid> {
    unsafe {
        if header.is_null() || (header as usize) % 4 != 0 || *(header as *const u32) != MH_MAGIC_64 {
            return None;
        }
        let ncmds = *(header.add(16) as *const u32);
        let mut cmd = header.add(32);
        for _ in 0..ncmds {
            let kind = *(cmd as *const u32);
            let size = *(cmd.add(4) as *const u32) as usize;
            if size < 8 {
                return None;
            }
            if kind == LC_UUID {
                let mut u = [0u8; 16];
                u.copy_from_slice(std::slice::from_raw_parts(cmd.add(8), 16));
                return Some(u);
            }
            cmd = cmd.add(size);
        }
        None
    }
}

pub fn loaded_images() -> Vec<Loaded> {
    let mut out = Vec::new();
    unsafe {
        for i in 0.._dyld_image_count() {
            let header = _dyld_get_image_header(i);
            if let Some(uuid) = image_uuid(header) {
                out.push(Loaded { header: header as usize, uuid });
            }
        }
    }
    out
}

/// The exported function containing `frame`, if this process has the very build (`uuid`) that crashed loaded.
pub fn resolve(images: &[Loaded], frame: &FrameRef) -> Option<Entry> {
    let image = images.iter().find(|i| i.uuid == frame.uuid)?;
    let address = image.header.checked_add(usize::try_from(frame.offset).ok()?)?;

    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    if unsafe { libc::dladdr(address as *const libc::c_void, &mut info) } == 0 || info.dli_sname.is_null() {
        return None;
    }
    // The symbol must be in the same image, else this address is not what we think it is.
    if info.dli_fbase as usize != image.header {
        return None;
    }
    let start = (info.dli_saddr as usize).checked_sub(image.header)? as u64;
    if frame.offset.checked_sub(start)? > MAX_REACH {
        return None;
    }
    let name = display_name(&unsafe { CStr::from_ptr(info.dli_sname) }.to_string_lossy());
    (!name.is_empty()).then_some(Entry { uuid: frame.uuid, start, end: frame.offset + 1, name })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame in a system library's exported function, described the way a dump would: UUID + offset.
    #[test]
    fn resolves_an_exported_function_by_uuid_and_offset() {
        let address = libc::getpid as *const () as usize;
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        assert_ne!(unsafe { libc::dladdr(address as *const libc::c_void, &mut info) }, 0);
        let header = info.dli_fbase as usize;
        let images = loaded_images();
        let image = images.iter().find(|i| i.header == header).expect("the image getpid lives in");

        let frame = FrameRef {
            thread: 1,
            uuid: image.uuid,
            offset: (address - header) as u64 + 4,
            path: "/usr/lib/system/libsystem_kernel.dylib".into(),
        };
        let entry = resolve(&images, &frame).expect("dladdr hit");
        // `getpid` has aliases at one address (`_getpid`, `__getpid`); dladdr may return either.
        assert!(entry.name.ends_with("getpid"), "{}", entry.name);
        assert!(entry.start <= frame.offset && frame.offset < entry.end);

        // A build this process does not have loaded resolves to nothing (never to a guess).
        let other = FrameRef { uuid: [0x5a; 16], ..frame };
        assert!(resolve(&images, &other).is_none());
    }

    #[test]
    fn a_far_neighbour_is_not_a_name() {
        let images = loaded_images();
        let image = images.first().expect("an image");
        // The very start of an image's text is a header, not a function: nothing exported sits within reach of an
        // absurd offset.
        let frame = FrameRef { thread: 1, uuid: image.uuid, offset: 0x7fff_0000, path: "/usr/lib/x".into() };
        assert!(resolve(&images, &frame).is_none());
    }
}
