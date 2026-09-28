//! Launch-time snapshot of the facts that need no reading at crash time: OS / app info and the
//! loaded-image list. Runs in normal code (allocation is fine) and is re-run when images load.

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::CStr;

use crate::snapshot::{ModuleDTO, SessionDTO};

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_EXECUTE: u32 = 2;
const LC_SEGMENT_64: u32 = 0x19;
const LC_UUID: u32 = 0x1b;

unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_header(index: u32) -> *const u8;
    fn _dyld_get_image_name(index: u32) -> *const libc::c_char;
}

fn sysctl_string(name: &CStr) -> String {
    let mut len = 0usize;
    unsafe {
        if libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 || len == 0 {
            return String::new();
        }
        let mut buf = vec![0u8; len];
        if libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) != 0 {
            return String::new();
        }
        buf.truncate(len);
        while buf.last() == Some(&0) {
            buf.pop();
        }
        String::from_utf8_lossy(&buf).into_owned()
    }
}

fn sysctl_u32(name: &CStr) -> u32 {
    let mut v = 0u32;
    let mut len = std::mem::size_of::<u32>();
    unsafe {
        libc::sysctlbyname(name.as_ptr(), (&mut v as *mut u32).cast(), &mut len, std::ptr::null_mut(), 0);
    }
    v
}

/// One loaded image: `(base, __TEXT vmsize, LC_UUID, is the main executable)`.
unsafe fn read_image(header: *const u8) -> Option<(u64, u64, [u8; 16], bool)> {
    if header.is_null() || (header as usize) % 4 != 0 || *(header as *const u32) != MH_MAGIC_64 {
        return None;
    }
    let filetype = *(header.add(12) as *const u32);
    let ncmds = *(header.add(16) as *const u32);
    let mut cmd = header.add(32);
    let (mut size, mut uuid) = (None, None);
    for _ in 0..ncmds {
        let kind = *(cmd as *const u32);
        let cmdsize = *(cmd.add(4) as *const u32) as usize;
        if cmdsize < 8 {
            return None;
        }
        match kind {
            LC_SEGMENT_64 if size.is_none() && std::slice::from_raw_parts(cmd.add(8), 7) == b"__TEXT\0" => {
                size = Some(*(cmd.add(32) as *const u64));
            }
            LC_UUID if uuid.is_none() => {
                let mut u = [0u8; 16];
                u.copy_from_slice(std::slice::from_raw_parts(cmd.add(8), 16));
                uuid = Some(u);
            }
            _ => {}
        }
        cmd = cmd.add(cmdsize);
    }
    Some((header as u64, size?, uuid?, filetype == MH_EXECUTE))
}

/// Every image dyld reports right now, the main executable first.
pub fn modules() -> Vec<ModuleDTO> {
    let mut out = Vec::new();
    unsafe {
        for i in 0.._dyld_image_count() {
            let header = _dyld_get_image_header(i);
            let Some((base, size, uuid, is_main)) = read_image(header) else { continue };
            let name = _dyld_get_image_name(i);
            let path = if name.is_null() { String::new() } else { CStr::from_ptr(name).to_string_lossy().into_owned() };
            out.push(ModuleDTO { base, size, uuid, is_main, path });
        }
    }
    out.sort_by_key(|m| !m.is_main);
    out
}

/// The current process's session record.
pub fn current(launch_id: [u8; 16], app_version: &str) -> SessionDTO {
    SessionDTO {
        launch_id,
        pid: std::process::id(),
        app_version: app_version.to_string(),
        os_version: sysctl_string(c"kern.osproductversion"),
        os_build: sysctl_string(c"kern.osversion"),
        machine: sysctl_string(c"hw.machine"),
        ncpu: sysctl_u32(c"hw.ncpu"),
        exe_path: std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        #[cfg(target_os = "ios")]
        build_info: crate::buildinfo::text().to_string(),
        #[cfg(not(target_os = "ios"))]
        build_info: String::new(),
        modules: modules(),
    }
}
