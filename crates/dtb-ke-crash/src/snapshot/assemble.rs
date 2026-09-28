//! Snapshot → minidump. The stream set and record layouts follow what `minidump-writer` emits for a
//! macOS process (arm64: `CONTEXT_ARM64_OLD`, the Breakpad Mach exception encoding, PDB70 module
//! records keyed by the Mach-O UUID), with `PlatformId::Ios`, so `minidump`, `minidump-unwind`,
//! `dtb-ke-debugger`, `lldb` and `minidump-stackwalk` treat it like any other Apple dump.

use minidump_common::format::{self as f, *};
use scroll::{Endian, Pwrite, ctx::SizeWith};

use super::*;

const LE: Endian = Endian::Little;
const EXC_BAD_ACCESS: u32 = 1;

struct Out {
    buf: Vec<u8>,
}

impl Out {
    fn pos(&self) -> u32 {
        self.buf.len() as u32
    }

    fn bytes(&mut self, b: &[u8]) -> u32 {
        let rva = self.pos();
        self.buf.extend_from_slice(b);
        rva
    }

    /// Append one `Pwrite` struct, returning its location.
    fn put<T>(&mut self, value: T) -> MINIDUMP_LOCATION_DESCRIPTOR
    where
        T: scroll::ctx::TryIntoCtx<Endian, Error = scroll::Error> + SizeWith<Endian>,
    {
        let size = T::size_with(&LE);
        let rva = self.pos();
        self.buf.resize(self.buf.len() + size, 0);
        self.buf[rva as usize..].pwrite_with(value, 0, LE).expect("sized write");
        MINIDUMP_LOCATION_DESCRIPTOR { data_size: size as u32, rva }
    }

    /// `MINIDUMP_STRING`: `u32` byte length, UTF-16LE, a NUL terminator.
    fn string(&mut self, s: &str) -> u32 {
        let units: Vec<u16> = s.encode_utf16().collect();
        let rva = self.pos();
        self.buf.extend_from_slice(&((units.len() * 2) as u32).to_le_bytes());
        for u in units.iter().chain(std::iter::once(&0)) {
            self.buf.extend_from_slice(&u.to_le_bytes());
        }
        rva
    }
}

fn context(t: &ThreadDTO) -> CONTEXT_ARM64_OLD {
    let r = &t.regs;
    let mut c = CONTEXT_ARM64_OLD::default();
    c.context_flags = ContextFlagsArm64Old::CONTEXT_ARM64_OLD_FULL.bits() as u64;
    c.iregs[..29].copy_from_slice(&r.x);
    c.iregs[29] = r.fp;
    c.iregs[30] = r.lr;
    c.sp = r.sp;
    c.pc = r.pc;
    c.cpsr = r.cpsr;
    c
}

fn os_version(s: &str) -> (u32, u32, u32) {
    let mut it = s.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Build the `.dtbkedmp`. Never fails: whatever the snapshot lacks (no sidecar, no stack, no exception) is
/// simply omitted from the dump.
pub fn to_minidump(snap: &CrashSnapshotDTO) -> Vec<u8> {
    let mut out = Out { buf: vec![0; 32] };
    let mut dir: Vec<MINIDUMP_DIRECTORY> = Vec::new();
    let mut stream = |dir: &mut Vec<MINIDUMP_DIRECTORY>, ty: MINIDUMP_STREAM_TYPE, loc: MINIDUMP_LOCATION_DESCRIPTOR| {
        dir.push(MINIDUMP_DIRECTORY { stream_type: ty as u32, location: loc });
    };

    // ── system info ──
    {
        let s = &snap.session;
        let (major, minor, patch) = os_version(&s.os_version);
        let csd = out.string(&s.os_build);
        let info = MINIDUMP_SYSTEM_INFO {
            processor_architecture: ProcessorArchitecture::PROCESSOR_ARCHITECTURE_ARM64_OLD as u16,
            processor_level: 0,
            processor_revision: 0,
            number_of_processors: s.ncpu.min(255) as u8,
            product_type: 1,
            major_version: major,
            minor_version: minor,
            build_number: patch,
            platform_id: PlatformId::Ios as u32,
            csd_version_rva: csd,
            suite_mask: 0,
            reserved2: 0,
            cpu: CPU_INFORMATION { data: [0; 24] },
        };
        let loc = out.put(info);
        stream(&mut dir, MINIDUMP_STREAM_TYPE::SystemInfoStream, loc);
    }

    // ── threads + their stacks + contexts ──
    let mut memory: Vec<MINIDUMP_MEMORY_DESCRIPTOR> = Vec::new();
    let mut raw_threads = Vec::new();
    for t in &snap.threads {
        let mut stack = MINIDUMP_MEMORY_DESCRIPTOR::default();
        if let Some(s) = &t.stack {
            let rva = out.bytes(&s.data);
            stack = MINIDUMP_MEMORY_DESCRIPTOR {
                start_of_memory_range: s.address,
                memory: MINIDUMP_LOCATION_DESCRIPTOR { data_size: s.data.len() as u32, rva },
            };
            memory.push(stack);
        }
        let ctx = out.put(context(t));
        raw_threads.push(MINIDUMP_THREAD {
            thread_id: t.id as u32,
            suspend_count: 0,
            priority_class: 0,
            priority: 0,
            teb: 0,
            stack,
            thread_context: ctx,
        });
    }
    {
        let start = out.pos();
        out.bytes(&(raw_threads.len() as u32).to_le_bytes());
        for t in raw_threads {
            out.put(t);
        }
        stream(&mut dir, MINIDUMP_STREAM_TYPE::ThreadListStream, MINIDUMP_LOCATION_DESCRIPTOR { data_size: out.pos() - start, rva: start });
    }

    // ── modules ──
    if !snap.session.modules.is_empty() {
        #[derive(Pwrite, scroll::SizeWith)]
        struct CvInfoPdb {
            cv_signature: u32,
            signature: GUID,
            age: u32,
        }
        let mut modules: Vec<&ModuleDTO> = snap.session.modules.iter().collect();
        modules.sort_by_key(|m| !m.is_main);
        let mut raw = Vec::new();
        for m in modules {
            let name = out.string(&m.path);
            let cv = out.put(CvInfoPdb {
                cv_signature: CvSignature::Pdb70 as u32,
                signature: m.uuid.into(),
                age: 0,
            });
            let leaf = m.path.rsplit('/').next().filter(|l| !l.is_empty()).unwrap_or("<Unknown>");
            out.bytes(leaf.as_bytes());
            out.bytes(&[0]);
            raw.push(MINIDUMP_MODULE {
                base_of_image: m.base,
                size_of_image: m.size as u32,
                module_name_rva: name,
                cv_record: MINIDUMP_LOCATION_DESCRIPTOR { data_size: cv.data_size + leaf.len() as u32 + 1, rva: cv.rva },
                ..Default::default()
            });
        }
        let start = out.pos();
        out.bytes(&(raw.len() as u32).to_le_bytes());
        for m in raw {
            out.put(m);
        }
        stream(&mut dir, MINIDUMP_STREAM_TYPE::ModuleListStream, MINIDUMP_LOCATION_DESCRIPTOR { data_size: out.pos() - start, rva: start });
    }

    // ── memory list ──
    if !memory.is_empty() {
        let start = out.pos();
        out.bytes(&(memory.len() as u32).to_le_bytes());
        for m in memory {
            out.put(m);
        }
        stream(&mut dir, MINIDUMP_STREAM_TYPE::MemoryListStream, MINIDUMP_LOCATION_DESCRIPTOR { data_size: out.pos() - start, rva: start });
    }

    // ── exception ──
    if let Some(e) = &snap.exception {
        let crashed = snap.threads.iter().find(|t| t.crashed || t.id == e.thread_id);
        let pc = crashed.map(|t| t.regs.pc).unwrap_or(0);
        let mut rec = MINIDUMP_EXCEPTION {
            exception_code: e.kind,
            exception_flags: e.code as u32,
            exception_address: if e.kind == EXC_BAD_ACCESS { e.subcode } else { pc },
            number_parameters: 3,
            ..Default::default()
        };
        rec.exception_information[0] = e.kind as u64;
        rec.exception_information[1] = e.code;
        rec.exception_information[2] = e.subcode;
        let thread_context = crashed.map(|t| out.put(context(t))).unwrap_or_default();
        let loc = out.put(MINIDUMP_EXCEPTION_STREAM {
            thread_id: e.thread_id as u32,
            __align: 0,
            exception_record: rec,
            thread_context,
        });
        stream(&mut dir, MINIDUMP_STREAM_TYPE::ExceptionStream, loc);
    }

    // ── thread names ──
    let named: Vec<&ThreadDTO> = snap.threads.iter().filter(|t| !t.name.is_empty()).collect();
    if !named.is_empty() {
        let names: Vec<(u32, u32)> = named.iter().map(|t| (t.id as u32, out.string(&t.name))).collect();
        let start = out.pos();
        out.bytes(&(names.len() as u32).to_le_bytes());
        for (id, rva) in names {
            out.put(MINIDUMP_THREAD_NAME { thread_id: id, thread_name_rva: rva as u64 });
        }
        stream(&mut dir, MINIDUMP_STREAM_TYPE::ThreadNamesStream, MINIDUMP_LOCATION_DESCRIPTOR { data_size: out.pos() - start, rva: start });
    }

    // ── build info (user stream) ──
    if !snap.session.build_info.is_empty() {
        let rva = out.bytes(snap.session.build_info.as_bytes());
        dir.push(MINIDUMP_DIRECTORY {
            stream_type: crate::buildinfo::STREAM_TYPE,
            location: MINIDUMP_LOCATION_DESCRIPTOR { data_size: snap.session.build_info.len() as u32, rva },
        });
        while out.buf.len() % 4 != 0 {
            out.buf.push(0);
        }
    }

    // ── directory + header ──
    let dir_rva = out.pos();
    for d in &dir {
        out.put(d.clone());
    }
    let header = MINIDUMP_HEADER {
        signature: MINIDUMP_SIGNATURE,
        version: MINIDUMP_VERSION,
        stream_count: dir.len() as u32,
        stream_directory_rva: dir_rva,
        checksum: 0,
        time_date_stamp: snap.crash_time_unix as u32,
        flags: 0,
    };
    out.buf[..].pwrite_with(header, 0, LE).expect("header");
    out.buf
}
