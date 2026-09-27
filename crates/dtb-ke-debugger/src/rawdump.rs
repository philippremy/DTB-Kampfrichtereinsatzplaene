//! The raw streams of a dump, as text — the "Raw dump" tab. Known streams reuse `minidump`'s own
//! `print` (the format `minidump_dump` prints), our build-info stream is shown verbatim, our system-symbol
//! stream is laid out per image, and anything else falls back to a hex preview.

use std::fmt::Write as _;
use std::ops::Deref;

use dtb_ke_crash::buildinfo::STREAM_TYPE as BUILD_INFO_STREAM;
use dtb_ke_crash::nsexception::STREAM_TYPE as NSEXCEPTION_STREAM;
use dtb_ke_crash::syshints::{Hints, Quality, STREAM_TYPE as SYSTEM_HINTS_STREAM};
use minidump::{
    Minidump, MinidumpAssertion, MinidumpBreakpadInfo, MinidumpCrashpadInfo, MinidumpException,
    MinidumpMemory64List, MinidumpMemoryInfoList, MinidumpMemoryList, MinidumpMiscInfo,
    MinidumpModuleList, MinidumpSystemInfo, MinidumpThreadList, MinidumpThreadNames,
    MinidumpUnloadedModuleList, Module,
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

/// The name of one of our own user streams (the dump format has no name field for those).
fn own_stream_name(type_id: u32) -> Option<&'static str> {
    match type_id {
        BUILD_INFO_STREAM => Some("BuildInfoStream"),
        SYSTEM_HINTS_STREAM => Some("SystemSymbolHintsStream"),
        NSEXCEPTION_STREAM => Some("NSExceptionStream"),
        _ => None,
    }
}

pub fn vendor(type_id: u32) -> &'static str {
    if own_stream_name(type_id).is_some() {
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
    own_stream_name(type_id).is_some()
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
                    None => match own_stream_name(s.stream_type) {
                        Some(name) => name.into(),
                        None => format!("{:#010x}", s.stream_type),
                    },
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

    if type_id == SYSTEM_HINTS_STREAM {
        return system_hints_text(dump);
    }

    if type_id == NSEXCEPTION_STREAM {
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

/// The system-symbol stream as a readable report: the header facts, then the collected names per image (the image's
/// name comes from the dump's module list, matched on its UUID).
fn system_hints_text<'a, T: Deref<Target = [u8]> + 'a>(dump: &Minidump<'a, T>) -> String {
    let bytes = match dump.get_raw_stream(SYSTEM_HINTS_STREAM) {
        Ok(bytes) => bytes,
        Err(e) => return format!("Could not read stream: {e}"),
    };
    let Some(hints) = Hints::parse(&String::from_utf8_lossy(bytes)) else {
        return format!(
            "This is not a system-symbol stream this debugger understands (it should start with \"dtb-ke syshints v1\").\n\n{}",
            hex_preview(dump, SYSTEM_HINTS_STREAM)
        );
    };

    let mut image_names: std::collections::HashMap<[u8; 16], String> =
        std::collections::HashMap::new();
    if let Ok(list) = dump.get_stream::<MinidumpModuleList>() {
        for module in list.iter() {
            if let Some(id) = module.debug_identifier() {
                let path = module.code_file();
                let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_owned();
                image_names.entry(*id.uuid().as_bytes()).or_insert(name);
            }
        }
    }

    let mut s = String::new();
    let _ = writeln!(
        s,
        "System symbols — names for OS-library frames, collected on the crashed machine\n"
    );
    let row = |s: &mut String, label: &str, value: String| {
        let _ = writeln!(s, "{label:<11} {value}");
    };
    row(&mut s, "Produced by", hints.producer.clone());
    row(
        &mut s,
        "Quality",
        match hints.quality {
            Quality::Exact => "exact — full local symbols of this exact OS build".into(),
            Quality::Approximate => {
                "approximate — exported symbols only; a private function shows as the nearest exported one before it".into()
            }
        },
    );
    row(
        &mut s,
        "Complete",
        if hints.complete {
            "yes".into()
        } else if hints.incomplete_reason.is_empty() {
            "no".into()
        } else {
            format!("no — {}", hints.incomplete_reason)
        },
    );
    let unnamed = hints.frames_total.saturating_sub(hints.frames_resolved);
    row(
        &mut s,
        "Frames",
        format!(
            "{} of {} system-library frames named ({unnamed} without a name)",
            hints.frames_resolved, hints.frames_total
        ),
    );
    row(&mut s, "OS build", hints.os_build.clone());

    let mut by_image: std::collections::BTreeMap<
        (String, [u8; 16]),
        Vec<&dtb_ke_crash::syshints::Entry>,
    > = std::collections::BTreeMap::new();
    for entry in &hints.entries {
        let name = image_names
            .get(&entry.uuid)
            .cloned()
            .unwrap_or_else(|| "(image not in the module list)".into());
        by_image.entry((name, entry.uuid)).or_default().push(entry);
    }
    row(
        &mut s,
        "Names",
        format!(
            "{} function{} in {} image{}",
            hints.entries.len(),
            plural(hints.entries.len()),
            by_image.len(),
            plural(by_image.len())
        ),
    );

    for ((name, uuid), mut entries) in by_image {
        entries.sort_by_key(|e| e.start);
        let _ = writeln!(
            s,
            "\n── {name} · {} · {} function{}",
            dtb_ke_crash::syshints::uuid_hex(&uuid),
            entries.len(),
            plural(entries.len())
        );
        for e in entries {
            let _ = writeln!(
                s,
                "   +{:#08x} … +{:#08x}   {}   ({} bytes)",
                e.start,
                e.end,
                e.name,
                e.end - e.start
            );
        }
    }
    s
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
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
