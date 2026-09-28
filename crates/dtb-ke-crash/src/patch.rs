//! Appending a stream to a finished minidump, per the format (`MINIDUMP_HEADER` / `MINIDUMP_DIRECTORY`).
//!
//! `minidump-writer` has no user-stream API on macOS / Linux, so the crash helper patches the dump
//! instead. The format lets us: the header names the directory by RVA and readers follow that, so
//! nothing requires the directory to sit right after the header. We append the new stream's data,
//! then a **fresh copy of the directory** (old entries + the new one) and repoint the header at it.
//! The old directory bytes stay behind as unreferenced padding, which every reader ignores; existing
//! streams keep their offsets, so nothing else moves. Streams and the directory are 4-byte aligned
//! (the writers' convention, and what Windows' own `MiniDumpWriteDump` produces).

use minidump_common::format::{
    MINIDUMP_DIRECTORY, MINIDUMP_HEADER, MINIDUMP_LOCATION_DESCRIPTOR, MINIDUMP_SIGNATURE,
};
use scroll::{Endian, Pread, Pwrite, ctx::SizeWith};

const LE: Endian = Endian::Little;

#[derive(Debug, PartialEq, Eq)]
pub enum PatchError {
    /// Not a minidump (short, bad signature).
    NotAMinidump,
    /// The header or directory points outside the file.
    Corrupt,
    /// The result would not fit the format's 32-bit offsets.
    TooLarge,
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PatchError::NotAMinidump => "not a minidump",
            PatchError::Corrupt => "minidump header/directory out of bounds",
            PatchError::TooLarge => "minidump would exceed 4 GiB",
        })
    }
}
impl std::error::Error for PatchError {}

fn pad4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

/// Add (or replace) the stream `stream_type` with `data`. On error `dmp` is untouched.
pub fn append_stream(dmp: &mut Vec<u8>, stream_type: u32, data: &[u8]) -> Result<(), PatchError> {
    let header: MINIDUMP_HEADER = dmp.pread_with(0, LE).map_err(|_| PatchError::NotAMinidump)?;
    if header.signature != MINIDUMP_SIGNATURE {
        return Err(PatchError::NotAMinidump);
    }

    let entry_size = MINIDUMP_DIRECTORY::size_with(&LE);
    let dir_len = (header.stream_count as usize)
        .checked_mul(entry_size)
        .ok_or(PatchError::Corrupt)?;
    let dir_start = header.stream_directory_rva as usize;
    if dir_start.checked_add(dir_len).is_none_or(|end| end > dmp.len()) {
        return Err(PatchError::Corrupt);
    }
    let mut dir: Vec<MINIDUMP_DIRECTORY> = (0..header.stream_count as usize)
        .map(|i| dmp.pread_with(dir_start + i * entry_size, LE).map_err(|_| PatchError::Corrupt))
        .collect::<Result<_, _>>()?;

    // Work on the tail only, so a failure leaves `dmp` as it was.
    let base = dmp.len();
    let mut tail: Vec<u8> = Vec::with_capacity(data.len() + dir.len() * entry_size + entry_size + 8);
    let pad_to = |len: usize| (4 - len % 4) % 4;
    tail.resize(pad_to(base), 0);

    let data_rva = base + tail.len();
    tail.extend_from_slice(data);
    tail.resize(tail.len() + pad_to(base + tail.len()), 0);

    let new = MINIDUMP_DIRECTORY {
        stream_type,
        location: MINIDUMP_LOCATION_DESCRIPTOR {
            data_size: u32::try_from(data.len()).map_err(|_| PatchError::TooLarge)?,
            rva: u32::try_from(data_rva).map_err(|_| PatchError::TooLarge)?,
        },
    };
    match dir.iter_mut().find(|d| d.stream_type == stream_type) {
        Some(existing) => *existing = new,
        None => dir.push(new),
    }

    let new_dir_rva = base + tail.len();
    let dir_bytes = dir.len() * entry_size;
    if new_dir_rva.checked_add(dir_bytes).is_none_or(|end| end > u32::MAX as usize) {
        return Err(PatchError::TooLarge);
    }
    let at = tail.len();
    tail.resize(at + dir_bytes, 0);
    for (i, entry) in dir.iter().enumerate() {
        tail.pwrite_with(entry.clone(), at + i * entry_size, LE).map_err(|_| PatchError::Corrupt)?;
    }

    let patched = MINIDUMP_HEADER {
        stream_count: dir.len() as u32,
        stream_directory_rva: new_dir_rva as u32,
        ..header
    };
    dmp.extend_from_slice(&tail);
    dmp.pwrite_with(patched, 0, LE).map_err(|_| PatchError::Corrupt)?;
    Ok(())
}
