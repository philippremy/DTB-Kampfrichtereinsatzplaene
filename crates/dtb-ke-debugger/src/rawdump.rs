//! The raw streams of a dump, as text — the "Raw dump" tab. Known streams reuse `minidump`'s own
//! `print` (the format `minidump_dump` prints), our build-info stream is shown verbatim, and anything
//! else falls back to a hex preview.

use std::fmt::Write as _;
use std::ops::Deref;

use dtb_ke_crash::buildinfo::STREAM_TYPE as BUILD_INFO_STREAM;
use minidump::{
    Minidump, MinidumpAssertion, MinidumpBreakpadInfo, MinidumpCrashpadInfo, MinidumpException,
    MinidumpMemory64List, MinidumpMemoryInfoList, MinidumpMemoryList, MinidumpMiscInfo,
    MinidumpModuleList, MinidumpSystemInfo, MinidumpThreadList, MinidumpThreadNames,
    MinidumpUnloadedModuleList,
};
use minidump_common::format::MINIDUMP_STREAM_TYPE;
use num_traits::FromPrimitive;

#[derive(Clone, Debug)]
pub struct StreamEntry {
    pub index: usize,
    pub type_id: u32,
    pub name: String,
    pub vendor: &'static str,
    pub size: u32,
    /// We can render it structurally (not just as a hex preview).
    pub understood: bool,
}

pub fn vendor(type_id: u32) -> &'static str {
    if type_id == BUILD_INFO_STREAM {
        return "DTB KE";
    }
    if type_id <= MINIDUMP_STREAM_TYPE::LastReservedStream as u32 {
        return "Official";
    }
    match type_id & 0xFFFF_0000 {
        0x4767_0000 => "Google",
        0x4d7a_0000 => "Mozilla",
        0x4350_0000 => "Crashpad",
        _ => "Unknown",
    }
}

fn is_understood(ty: Option<MINIDUMP_STREAM_TYPE>, type_id: u32) -> bool {
    use MINIDUMP_STREAM_TYPE::*;
    type_id == BUILD_INFO_STREAM
        || matches!(
            ty,
            Some(
                SystemInfoStream
                    | MiscInfoStream
                    | ThreadNamesStream
                    | ThreadListStream
                    | AssertionInfoStream
                    | BreakpadInfoStream
                    | CrashpadInfoStream
                    | ExceptionStream
                    | ModuleListStream
                    | UnloadedModuleListStream
                    | MemoryListStream
                    | Memory64ListStream
                    | MemoryInfoListStream
                    | LinuxCmdLine
                    | LinuxMaps
                    | LinuxCpuInfo
                    | LinuxEnviron
                    | LinuxLsbRelease
                    | LinuxProcStatus
            )
        )
}

pub fn streams<'a, T: Deref<Target = [u8]> + 'a>(dump: &Minidump<'a, T>) -> Vec<StreamEntry> {
    dump.all_streams()
        .enumerate()
        .map(|(index, s)| {
            let ty = MINIDUMP_STREAM_TYPE::from_u32(s.stream_type);
            StreamEntry {
                index,
                type_id: s.stream_type,
                name: match ty {
                    Some(t) => format!("{t:?}"),
                    None if s.stream_type == BUILD_INFO_STREAM => "BuildInfoStream".into(),
                    None => format!("{:#010x}", s.stream_type),
                },
                vendor: vendor(s.stream_type),
                size: s.location.data_size,
                understood: is_understood(ty, s.stream_type),
            }
        })
        .collect()
}

/// The stream's text (empty-handed streams say why).
pub fn stream_text<'a, T: Deref<Target = [u8]> + 'a>(
    dump: &Minidump<'a, T>,
    type_id: u32,
) -> String {
    use MINIDUMP_STREAM_TYPE::*;
    let mut out = Vec::new();
    let ty = MINIDUMP_STREAM_TYPE::from_u32(type_id);
    let system = dump.get_stream::<MinidumpSystemInfo>();
    let misc = dump.get_stream::<MinidumpMiscInfo>();

    macro_rules! print {
        ($t:ty, |$s:ident| $call:expr) => {
            match dump.get_stream::<$t>() {
                Ok($s) => {
                    let _ = $call;
                }
                Err(e) => return format!("Could not read stream: {e}"),
            }
        };
    }

    if type_id == BUILD_INFO_STREAM {
        return dump
            .get_raw_stream(type_id)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|e| format!("Could not read stream: {e}"));
    }

    match ty {
        Some(SystemInfoStream) => print!(MinidumpSystemInfo, |s| s.print(&mut out)),
        Some(MiscInfoStream) => print!(MinidumpMiscInfo, |s| s.print(&mut out)),
        Some(ThreadNamesStream) => print!(MinidumpThreadNames, |s| s.print(&mut out)),
        Some(ThreadListStream) => {
            let memory = dump.get_memory();
            print!(MinidumpThreadList, |s| s.print(
                &mut out,
                memory.as_ref(),
                system.as_ref().ok(),
                misc.as_ref().ok(),
                true
            ))
        }
        Some(AssertionInfoStream) => print!(MinidumpAssertion, |s| s.print(&mut out)),
        Some(BreakpadInfoStream) => print!(MinidumpBreakpadInfo, |s| s.print(&mut out)),
        Some(CrashpadInfoStream) => print!(MinidumpCrashpadInfo, |s| s.print(&mut out)),
        Some(ExceptionStream) => {
            print!(MinidumpException, |s| s.print(
                &mut out,
                system.as_ref().ok(),
                misc.as_ref().ok()
            ))
        }
        Some(ModuleListStream) => print!(MinidumpModuleList, |s| s.print(&mut out)),
        Some(UnloadedModuleListStream) => print!(MinidumpUnloadedModuleList, |s| s.print(&mut out)),
        Some(MemoryListStream) => print!(MinidumpMemoryList, |s| s.print(&mut out, true)),
        Some(Memory64ListStream) => print!(MinidumpMemory64List, |s| s.print(&mut out, true)),
        Some(MemoryInfoListStream) => print!(MinidumpMemoryInfoList, |s| s.print(&mut out)),
        Some(
            LinuxCmdLine | LinuxMaps | LinuxCpuInfo | LinuxEnviron | LinuxLsbRelease
            | LinuxProcStatus,
        ) => {
            return match dump.get_raw_stream(type_id) {
                // `/proc` style streams: NUL-separated in the environ / cmdline case.
                Ok(b) => String::from_utf8_lossy(b).replace('\0', "\\0\n"),
                Err(e) => format!("Could not read stream: {e}"),
            };
        }
        _ => return hex_preview(dump, type_id),
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_preview<'a, T: Deref<Target = [u8]> + 'a>(dump: &Minidump<'a, T>, type_id: u32) -> String {
    let Ok(bytes) = dump.get_raw_stream(type_id) else {
        return "Could not read stream.".into();
    };
    let mut s = format!(
        "{} bytes, no structured view — hex dump (max. 4 KiB):\n\n",
        bytes.len()
    );
    for (i, row) in bytes.chunks(16).take(256).enumerate() {
        let _ = write!(s, "{:08x}  ", i * 16);
        for b in row {
            let _ = write!(s, "{b:02x} ");
        }
        for _ in row.len()..16 {
            s.push_str("   ");
        }
        s.push(' ');
        s.extend(row.iter().map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            }
        }));
        s.push('\n');
    }
    s
}
