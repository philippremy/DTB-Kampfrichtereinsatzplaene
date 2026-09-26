use super::wire::*;
use super::*;

#[derive(Debug)]
pub enum SnapshotError {
    BadMagic,
    UnsupportedVersion(u32),
    /// The mandatory header record is missing (an empty / truncated-at-the-start file).
    NoHeader,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not a crash snapshot file"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported snapshot version {v}"),
            Self::NoHeader => write!(f, "snapshot has no header record"),
        }
    }
}
impl std::error::Error for SnapshotError {}

/// Cursor over the records of one file.
struct Records<'a> {
    rest: &'a [u8],
}

impl<'a> Records<'a> {
    fn open(bytes: &'a [u8], magic: &[u8; 8]) -> Result<Self, SnapshotError> {
        if bytes.len() < 16 || &bytes[..8] != magic {
            return Err(SnapshotError::BadMagic);
        }
        let version = u32_at(bytes, 8);
        if version != FORMAT_VERSION {
            return Err(SnapshotError::UnsupportedVersion(version));
        }
        Ok(Self { rest: &bytes[16..] })
    }
}

impl<'a> Iterator for Records<'a> {
    type Item = (u16, &'a [u8]);

    /// A record cut off mid-payload ends iteration (the tail of a file the watchdog killed).
    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.len() < 8 {
            return None;
        }
        let tag = u16::from_le_bytes([self.rest[0], self.rest[1]]);
        let len = u32_at(self.rest, 4) as usize;
        let body = self.rest.get(8..8 + len)?;
        self.rest = &self.rest[8 + len..];
        Some((tag, body))
    }
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Parse the `.crash` file and its `.session` sidecar (`None` = the sidecar is gone; the dump is
/// then written without a module list).
pub fn parse(crash: &[u8], session: Option<&[u8]>) -> Result<CrashSnapshotDTO, SnapshotError> {
    let mut snap = CrashSnapshotDTO::default();
    let mut header = false;

    for (tag, body) in Records::open(crash, CRASH_MAGIC)? {
        match tag {
            TAG_HEADER if body.len() >= HEADER_PAYLOAD => {
                snap.launch_id.copy_from_slice(&body[..16]);
                snap.pid = u32_at(body, 16);
                snap.crash_time_unix = u64_at(body, 24);
                header = true;
            }
            TAG_EXCEPTION if body.len() >= EXCEPTION_PAYLOAD => {
                snap.exception = Some(ExceptionDTO {
                    kind: u32_at(body, 0),
                    code: u64_at(body, 8),
                    subcode: u64_at(body, 16),
                    thread_id: u64_at(body, 24),
                });
            }
            TAG_PANIC => snap.panic_message = Some(cstr(body)).filter(|m| !m.is_empty()),
            TAG_THREAD if body.len() >= THREAD_PAYLOAD => {
                let mut regs = Arm64RegsDTO::default();
                let r = 16;
                for (i, x) in regs.x.iter_mut().enumerate() {
                    *x = u64_at(body, r + i * 8);
                }
                regs.fp = u64_at(body, r + 29 * 8);
                regs.lr = u64_at(body, r + 30 * 8);
                regs.sp = u64_at(body, r + 31 * 8);
                regs.pc = u64_at(body, r + 32 * 8);
                regs.cpsr = u64_at(body, r + 33 * 8) as u32;
                snap.threads.push(ThreadDTO {
                    id: u64_at(body, 0),
                    crashed: u32_at(body, 8) & THREAD_FLAG_CRASHED != 0,
                    name: cstr(&body[r + ARM64_REGS * 8..r + ARM64_REGS * 8 + THREAD_NAME_LEN]),
                    regs,
                    stack: None,
                });
            }
            TAG_STACK if body.len() >= 16 => {
                let id = u64_at(body, 0);
                if let Some(t) = snap.threads.iter_mut().find(|t| t.id == id) {
                    t.stack = Some(StackDTO { address: u64_at(body, 8), data: body[16..].to_vec() });
                }
            }
            TAG_END => snap.complete = true,
            _ => {}
        }
    }
    if !header {
        return Err(SnapshotError::NoHeader);
    }
    if let Some(bytes) = session {
        if let Ok(s) = parse_session(bytes) {
            snap.session = s;
        }
    }
    Ok(snap)
}

pub(super) fn parse_session(bytes: &[u8]) -> Result<SessionDTO, SnapshotError> {
    let mut s = SessionDTO::default();
    for (tag, body) in Records::open(bytes, SESSION_MAGIC)? {
        match tag {
            TAG_SESSION_HEADER if body.len() >= 24 => {
                s.launch_id.copy_from_slice(&body[..16]);
                s.pid = u32_at(body, 16);
            }
            TAG_KV => {
                let text = cstr(body);
                let value_at = body.iter().position(|&c| c == 0).map(|i| i + 1).unwrap_or(body.len());
                let value = cstr(&body[value_at.min(body.len())..]);
                match text.as_str() {
                    "app_version" => s.app_version = value,
                    "os_version" => s.os_version = value,
                    "os_build" => s.os_build = value,
                    "machine" => s.machine = value,
                    "ncpu" => s.ncpu = value.parse().unwrap_or(0),
                    "exe_path" => s.exe_path = value,
                    _ => {}
                }
            }
            TAG_MODULE if body.len() >= MODULE_FIXED => {
                s.modules.push(ModuleDTO {
                    base: u64_at(body, 0),
                    size: u64_at(body, 8),
                    uuid: body[16..32].try_into().unwrap(),
                    is_main: u32_at(body, 32) & MODULE_FLAG_MAIN != 0,
                    path: String::from_utf8_lossy(&body[MODULE_FIXED..]).into_owned(),
                });
            }
            _ => {}
        }
    }
    Ok(s)
}
