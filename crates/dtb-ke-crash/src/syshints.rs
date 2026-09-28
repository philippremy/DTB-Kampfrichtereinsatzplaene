//! **System-symbol hints**: names for the frames a crash had in OS libraries, computed *on the machine that
//! crashed* — the only place that reliably has the exact build's symbols — and carried in the dump as a user
//! stream, so any machine can read them later (Apple's crash reports work the same way).
//!
//! Frames are keyed by the image's UUID and the offset into it, never by address, so the entries stay valid
//! however the image was loaded. Each entry is a *function range* (`start..end`), not a single address: the
//! debugger asks about slightly different addresses than the crash side recorded (return address vs call site),
//! and a range answers both.
//!
//! The stream also says **how good** it is, because two producers differ:
//!
//! * `exact` — macOS: read straight from the dyld shared cache with the images' local symbols (ObjC methods,
//!   private functions), the same data the debugger would use.
//! * `approximate` — iOS: only `dladdr`, i.e. exported symbols. A private function shows up as the nearest
//!   exported one before it. The debugger marks these and asks for the device-support files to refine them.
//!
//! and whether it ran to the end (`complete`), since the user can skip the resolution in the crash dialog.
//!
//! Text format, one record per line (readable with `strings`, extendable with new header keys):
//!
//! ```text
//! dtb-ke syshints v1
//! producer=macos-dyld-cache
//! quality=exact
//! complete=true
//! incomplete_reason=
//! frames_total=93
//! frames_resolved=71
//! os_build=26A428
//! --
//! 727e905ef34438238e13e7a096388321 1a2b0 1a2f4 _CFRunLoopRun
//! ```

use std::fmt::Write as _;

/// The minidump stream type (`DTKF`), next to `buildinfo::STREAM_TYPE` (`DTKE`).
pub const STREAM_TYPE: u32 = 0x4454_4b46;

const MAGIC: &str = "dtb-ke syshints v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// Full local symbols of the exact build (macOS, from the dyld shared cache).
    Exact,
    /// Exported symbols only (iOS, `dladdr`): the nearest one before the address, possibly the wrong function.
    Approximate,
}

/// One OS-library function: image UUID, and the `start..end` byte range of it within the image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub uuid: [u8; 16],
    pub start: u64,
    pub end: u64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hints {
    /// Who produced it (`macos-dyld-cache`, `ios-dladdr`).
    pub producer: String,
    pub quality: Quality,
    /// The producer ran through every frame. `false` when the user skipped, or it failed / had no data.
    pub complete: bool,
    /// Why not complete (`skipped`, `no-cache`, `error: …`); empty when complete.
    pub incomplete_reason: String,
    /// System frames asked about, and how many got a name.
    pub frames_total: u32,
    pub frames_resolved: u32,
    /// The OS build the names are valid for (`26A428`).
    pub os_build: String,
    pub entries: Vec<Entry>,
}

/// A frame to symbolicate: which thread, which image (UUID + install path) and the offset into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameRef {
    pub thread: u64,
    pub uuid: [u8; 16],
    pub offset: u64,
    /// The image's path as the crashed process saw it.
    pub path: String,
}

pub fn uuid_hex(uuid: &[u8; 16]) -> String {
    uuid.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_uuid(hex: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = hex.bytes().filter(|b| *b != b'-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, pair) in hex.chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

impl Hints {
    pub fn encode(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{MAGIC}");
        let _ = writeln!(s, "producer={}", self.producer);
        let _ = writeln!(s, "quality={}", if self.quality == Quality::Exact { "exact" } else { "approximate" });
        let _ = writeln!(s, "complete={}", self.complete);
        let _ = writeln!(s, "incomplete_reason={}", self.incomplete_reason.replace('\n', " "));
        let _ = writeln!(s, "frames_total={}", self.frames_total);
        let _ = writeln!(s, "frames_resolved={}", self.frames_resolved);
        let _ = writeln!(s, "os_build={}", self.os_build);
        let _ = writeln!(s, "--");
        for e in &self.entries {
            let _ = writeln!(s, "{} {:x} {:x} {}", uuid_hex(&e.uuid), e.start, e.end, e.name.replace('\n', " "));
        }
        s
    }

    /// `None` for anything that is not a v1 hints stream. Unknown header keys are ignored, malformed entry
    /// lines skipped — a damaged tail must not cost the readable part.
    pub fn parse(text: &str) -> Option<Hints> {
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let mut h = Hints {
            producer: String::new(),
            quality: Quality::Approximate,
            complete: false,
            incomplete_reason: String::new(),
            frames_total: 0,
            frames_resolved: 0,
            os_build: String::new(),
            entries: Vec::new(),
        };
        for line in lines.by_ref() {
            if line == "--" {
                break;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "producer" => h.producer = v.to_owned(),
                "quality" => h.quality = if v == "exact" { Quality::Exact } else { Quality::Approximate },
                "complete" => h.complete = v == "true",
                "incomplete_reason" => h.incomplete_reason = v.to_owned(),
                "frames_total" => h.frames_total = v.parse().unwrap_or(0),
                "frames_resolved" => h.frames_resolved = v.parse().unwrap_or(0),
                "os_build" => h.os_build = v.to_owned(),
                _ => {}
            }
        }
        for line in lines {
            let mut parts = line.splitn(4, ' ');
            let (Some(u), Some(a), Some(b), Some(name)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let (Some(uuid), Ok(start), Ok(end)) = (parse_uuid(u), u64::from_str_radix(a, 16), u64::from_str_radix(b, 16))
            else {
                continue;
            };
            h.entries.push(Entry { uuid, start, end, name: name.to_owned() });
        }
        Some(h)
    }

    /// The entry whose range contains `offset` in the image `uuid`.
    pub fn lookup(&self, uuid: &[u8; 16], offset: u64) -> Option<&Entry> {
        self.entries.iter().find(|e| &e.uuid == uuid && (e.start..e.end).contains(&offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Hints {
        Hints {
            producer: "macos-dyld-cache".into(),
            quality: Quality::Exact,
            complete: false,
            incomplete_reason: "skipped".into(),
            frames_total: 9,
            frames_resolved: 4,
            os_build: "26A428".into(),
            entries: vec![
                Entry { uuid: [0xab; 16], start: 0x100, end: 0x180, name: "-[NSApplication run]".into() },
                Entry { uuid: [0xcd; 16], start: 0x2000, end: 0x2040, name: "dispatch_client_callout".into() },
            ],
        }
    }

    #[test]
    fn round_trips_including_names_with_spaces_and_the_status_flags() {
        let h = sample();
        assert_eq!(Hints::parse(&h.encode()), Some(h));
    }

    #[test]
    fn lookup_is_by_uuid_and_range() {
        let h = sample();
        assert_eq!(h.lookup(&[0xab; 16], 0x100).map(|e| e.name.as_str()), Some("-[NSApplication run]"));
        assert_eq!(h.lookup(&[0xab; 16], 0x17f).map(|e| e.name.as_str()), Some("-[NSApplication run]"));
        assert!(h.lookup(&[0xab; 16], 0x180).is_none(), "end is exclusive");
        assert!(h.lookup(&[0xcd; 16], 0x100).is_none(), "wrong image");
    }

    #[test]
    fn a_damaged_or_foreign_stream_costs_only_what_is_unreadable() {
        assert!(Hints::parse("not hints").is_none());
        let mut text = sample().encode();
        text.push_str("garbage line\nzz 1 2 name\n");
        text = text.replace("frames_total=9", "future_key=1\nframes_total=9");
        let h = Hints::parse(&text).unwrap();
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.frames_total, 9);
        assert!(!h.complete);
    }
}
