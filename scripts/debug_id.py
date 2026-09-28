#!/usr/bin/env python3
"""Compute the Breakpad-style debug id(s) of a Mach-O / ELF / PE / PDB file.

Ports `dtb-ke-symid` (crates/dtb-ke-symid) to a dependency-free script, so CI's debug-info-stripping
and symbol-upload steps don't need to build a second Rust binary just for this one computation. The
byte-level algorithm is taken directly from the crates `dtb-ke-symid` actually calls through
(`samply_symbols::debug_id_for_object` and the `debugid` crate's `DebugId`/`BreakpadFormat`, both
read from the local cargo registry checkout while writing this), not reimplemented from memory:

- Mach-O:  the 16 raw bytes of the LC_UUID load command, verbatim, formatted uppercase + "0"
           (`DebugId::from_uuid` -> `from_parts(uuid, 0)`, no byte swap of any kind).
- ELF:     the first 16 bytes of the NT_GNU_BUILD_ID note (zero-padded if shorter), reinterpreted as
           a little-endian-fielded UUID and re-serialized as one (`DebugIdExt::from_identifier`) --
           this is the standard "Microsoft GUID" mixed-endian swap: bytes [0..4) reverse, [4..6)
           reverse, [6..8) reverse, [8..16) unchanged -- then appendix 0.
- PE:      the CodeView (PDB70/"RSDS") debug directory entry's 16-byte GUID, the same mixed-endian
           swap as ELF (`DebugId::from_guid_age`), appendix = age (not always 0).
- PDB:     the PDB info stream's GUID (same swap) + age. Not used by this project directly (Windows
           debug files here are split PE executables, not standalone .pdb), included for completeness.

Formatting (`BreakpadFormat`): uppercase hex of the 16 (post-swap) bytes, immediately followed by
*lowercase* hex of the appendix with no padding -- e.g. "1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E1".

Usage:
    debug_id.py <file>                 print each identity's debug id, one per line
    debug_id.py <file> --json          print [{"debug_id": ..., "file_type": ...}, ...]

A file this can't identify (wrong magic, truncated, no build id / debug directory found) prints
nothing and exits 1 -- callers that need "skip files with nothing to identify" behavior (matching
strip.rs/symbols.rs's own warnings, not hard failures) should check the exit code, not parse stderr.
"""

from __future__ import annotations

import json
import struct
import sys
import uuid as uuid_mod
from dataclasses import dataclass
from pathlib import Path


def _swap_guid(raw: bytes) -> bytes:
    """The Microsoft mixed-endian GUID -> big-endian UUID swap `DebugId::from_guid_age` /
    `DebugIdExt::from_identifier` both apply: reverse the first 4 bytes, then the next 2, then the
    next 2; the last 8 bytes are untouched either way."""
    if len(raw) != 16:
        raise ValueError(f"expected 16 bytes, got {len(raw)}")
    return bytes([raw[3], raw[2], raw[1], raw[0], raw[5], raw[4], raw[7], raw[6]]) + raw[8:16]


def _breakpad(uuid_bytes: bytes, appendix: int) -> str:
    if len(uuid_bytes) != 16:
        raise ValueError(f"expected 16 bytes, got {len(uuid_bytes)}")
    return f"{uuid_mod.UUID(bytes=uuid_bytes).hex.upper()}{appendix:x}"


@dataclass
class FileIdentity:
    debug_id: str
    file_type: str  # "MachO" | "Elf" | "Pe" | "Pdb"


# --- Mach-O ------------------------------------------------------------------------------------

#
# We always read the candidate magic with `>I` (explicit big-endian), regardless of host. MH_MAGIC(_64)
# means "this value, read as big-endian, equals the magic exactly" -> the rest of the file's fields
# are *also* big-endian (the read matched with no swap needed). MH_CIGAM(_64) is the byte-swapped
# constant: seeing *this* value under a big-endian read means the actual on-disk fields are
# little-endian (every modern Apple target -- arm64, x86_64 -- lands here).
_MACHO_MAGICS = {
    0xFEEDFACE: (False, ">"),  # MH_MAGIC (32-bit, big-endian file)
    0xFEEDFACF: (True, ">"),  # MH_MAGIC_64 (big-endian file)
    0xCEFAEDFE: (False, "<"),  # MH_CIGAM (32-bit, little-endian file)
    0xCFFAEDFE: (True, "<"),  # MH_CIGAM_64 (little-endian file -- arm64/x86_64 land here)
}
_FAT_MAGIC = 0xCAFEBABE
_FAT_CIGAM = 0xBEBAFECA
_LC_UUID = 0x1B


def _macho_uuid_from_slice(data: bytes) -> str | None:
    magic = struct.unpack_from(">I", data, 0)[0]
    info = _MACHO_MAGICS.get(magic)
    if info is None:
        return None
    is64, endian = info
    # mach_header(_64): magic, cputype, cpusubtype, filetype, ncmds, sizeofcmds, flags[, reserved]
    header_fmt = f"{endian}IiiIIII" + ("I" if is64 else "")
    header_size = struct.calcsize(header_fmt)
    _, _, _, _, ncmds, _, _flags, *_ = struct.unpack_from(header_fmt, data, 0)
    offset = header_size
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from(f"{endian}II", data, offset)
        if cmd == _LC_UUID:
            return _breakpad(data[offset + 8 : offset + 24], 0)
        offset += cmdsize
    return None


def identify_macho(data: bytes) -> list[FileIdentity]:
    magic = struct.unpack_from(">I", data, 0)[0]
    if magic in (_FAT_MAGIC, _FAT_CIGAM):
        endian = ">" if magic == _FAT_MAGIC else "<"
        (nfat,) = struct.unpack_from(f"{endian}I", data, 4)
        ids = []
        for i in range(nfat):
            # fat_arch: cputype, cpusubtype, offset, size, align (5x uint32, always big-endian per spec)
            _cputype, _cpusubtype, offset, size, _align = struct.unpack_from(
                ">iiIII", data, 8 + i * 20
            )
            debug_id = _macho_uuid_from_slice(data[offset : offset + size])
            if debug_id:
                ids.append(FileIdentity(debug_id, "MachO"))
        return ids
    debug_id = _macho_uuid_from_slice(data)
    return [FileIdentity(debug_id, "MachO")] if debug_id else []


# --- ELF -----------------------------------------------------------------------------------------

_NT_GNU_BUILD_ID = 3


@dataclass
class _ElfSection:
    name: str
    sh_type: int
    offset: int
    size: int


def _elf_endian_and_class(data: bytes) -> tuple[str, bool]:
    little_endian = data[5] == 1  # EI_DATA: 1 = little-endian, 2 = big-endian
    return ("<" if little_endian else ">"), (data[4] == 2)  # EI_CLASS: 2 = 64-bit


def _elf_sections(data: bytes) -> list[_ElfSection]:
    endian, is64 = _elf_endian_and_class(data)
    if is64:
        (e_shoff,) = struct.unpack_from(f"{endian}Q", data, 0x28)
        e_shentsize, e_shnum, e_shstrndx = struct.unpack_from(f"{endian}HHH", data, 0x3A)
    else:
        (e_shoff,) = struct.unpack_from(f"{endian}I", data, 0x20)
        e_shentsize, e_shnum, e_shstrndx = struct.unpack_from(f"{endian}HHH", data, 0x2E)

    def raw(i: int) -> tuple[int, int, int, int]:
        entry = data[e_shoff + i * e_shentsize : e_shoff + (i + 1) * e_shentsize]
        if is64:
            sh_name, sh_type, _flags, _addr, sh_offset, sh_size = struct.unpack_from(
                f"{endian}IIQQQQ", entry, 0
            )
        else:
            sh_name, sh_type, _flags, _addr, sh_offset, sh_size = struct.unpack_from(
                f"{endian}IIIIII", entry, 0
            )
        return sh_name, sh_type, sh_offset, sh_size

    strtab_name_off, _, strtab_offset, _ = raw(e_shstrndx) if e_shnum else (0, 0, 0, 0)
    del strtab_name_off

    sections = []
    for i in range(e_shnum):
        sh_name, sh_type, sh_offset, sh_size = raw(i)
        name = ""
        if strtab_offset:
            end = data.index(b"\0", strtab_offset + sh_name)
            name = data[strtab_offset + sh_name : end].decode("ascii", "replace")
        sections.append(_ElfSection(name, sh_type, sh_offset, sh_size))
    return sections


def debug_section_bytes(path: Path) -> int:
    """Total size of the `.debug_*`/`.zdebug_*` sections -- lets a caller verify a split debug file
    still carries all of the original binary's DWARF (mirrors `dtb_ke_symid::debug_section_bytes`,
    which sums via the `object` crate's generic section iterator across ELF/Mach-O/PE alike; here
    split by format since we have no such generic abstraction)."""
    data = path.read_bytes()
    if len(data) < 16:
        return 0
    if data[:4] == b"\x7fELF":
        return sum(
            s.size for s in _elf_sections(data) if s.name.startswith((".debug_", ".zdebug_"))
        )
    if data[:2] == b"MZ":
        return sum(
            size for name, size in _pe_sections(data) if name.startswith((".debug_", ".zdebug_"))
        )
    return 0


def identify_elf(data: bytes) -> list[FileIdentity]:
    for section in _elf_sections(data):
        if section.sh_type != 7:  # SHT_NOTE
            continue
        notes = data[section.offset : section.offset + section.size]
        endian, _ = _elf_endian_and_class(data)
        pos = 0
        while pos + 12 <= len(notes):
            namesz, descsz, note_type = struct.unpack_from(f"{endian}III", notes, pos)
            pos += 12
            name_end = pos + namesz
            name = notes[pos:name_end]
            pos = _align4(name_end)
            desc_end = pos + descsz
            desc = notes[pos:desc_end]
            pos = _align4(desc_end)
            if note_type == _NT_GNU_BUILD_ID and name.rstrip(b"\0") == b"GNU":
                build_id = desc[:16].ljust(16, b"\0")
                d1, d2, d3 = struct.unpack_from(f"{endian}IHH", build_id, 0)
                swapped = struct.pack(">IHH", d1, d2, d3) + build_id[8:16]
                return [FileIdentity(_breakpad(swapped, 0), "Elf")]
    return []


def _align4(n: int) -> int:
    return (n + 3) & ~3


# --- PE / PDB --------------------------------------------------------------------------------

_IMAGE_DEBUG_TYPE_CODEVIEW = 2


def identify_pe(data: bytes) -> list[FileIdentity]:
    (e_lfanew,) = struct.unpack_from("<I", data, 0x3C)
    if data[e_lfanew : e_lfanew + 4] != b"PE\0\0":
        return []
    coff_off = e_lfanew + 4
    _machine, num_sections, _ts, _symtab, _numsyms, opt_size, _chars = struct.unpack_from(
        "<HHIIIHH", data, coff_off
    )
    opt_off = coff_off + 20
    (magic,) = struct.unpack_from("<H", data, opt_off)
    is_pe32_plus = magic == 0x20B
    # The data directory count sits right before the array; the debug entry is index 6.
    num_dirs_off = opt_off + (108 if is_pe32_plus else 92)
    (num_dirs,) = struct.unpack_from("<I", data, num_dirs_off)
    if num_dirs <= 6:
        return []
    dir_array_off = num_dirs_off + 4
    debug_rva, debug_size = struct.unpack_from("<II", data, dir_array_off + 6 * 8)
    if debug_size == 0:
        return []

    ids: list[FileIdentity] = []
    entry_count = debug_size // 28
    # The debug directory entries themselves are read by RVA==file-offset only when unsectioned;
    # normally we must resolve RVA -> file offset via the section table, but each 28-byte
    # IMAGE_DEBUG_DIRECTORY entry already carries PointerToRawData (a real file offset) alongside
    # AddressOfRawData (the RVA), so no section walk is needed here.
    debug_dir_file_off = _rva_to_file_offset(data, coff_off, num_sections, debug_rva)
    if debug_dir_file_off is None:
        return []
    for i in range(entry_count):
        entry = data[debug_dir_file_off + i * 28 : debug_dir_file_off + (i + 1) * 28]
        if len(entry) < 28:
            break
        _characteristics, _ts, _maj, _min, dtype, _size, _rva, ptr = struct.unpack_from(
            "<IIHHIIII", entry, 0
        )
        if dtype != _IMAGE_DEBUG_TYPE_CODEVIEW:
            continue
        cv = data[ptr : ptr + 24]
        if cv[0:4] != b"RSDS":
            continue
        guid = cv[4:20]
        (age,) = struct.unpack_from("<I", cv, 20)
        ids.append(FileIdentity(_breakpad(_swap_guid(guid), age), "Pe"))
    return ids


def _rva_to_file_offset(data: bytes, coff_off: int, num_sections: int, rva: int) -> int | None:
    for _name, _vaddr, rawsize, rawptr in _pe_section_headers(data, coff_off, num_sections):
        if _vaddr <= rva < _vaddr + rawsize:
            return rawptr + (rva - _vaddr)
    return None


def _pe_section_headers(
    data: bytes, coff_off: int, num_sections: int
) -> list[tuple[bytes, int, int, int]]:
    """Raw `(name, VirtualAddress, SizeOfRawData, PointerToRawData)` per section (40-byte
    IMAGE_SECTION_HEADER entries, right after the optional header)."""
    sections_off = coff_off + 20 + struct.unpack_from("<H", data, coff_off + 16)[0]
    headers = []
    for i in range(num_sections):
        sec = data[sections_off + i * 40 : sections_off + (i + 1) * 40]
        if len(sec) < 40:
            break
        name, _vsize, vaddr, rawsize, rawptr = struct.unpack_from("<8sIIII", sec, 0)
        headers.append((name, vaddr, rawsize, rawptr))
    return headers


def _pe_sections(data: bytes) -> list[tuple[str, int]]:
    """`(name, SizeOfRawData)` per section -- used by `debug_section_bytes`.

    A section name longer than 8 bytes (every DWARF `.debug_*` name is) can't fit
    `IMAGE_SECTION_HEADER::Name` directly -- COFF stores `/<decimal offset>` instead, pointing into
    the string table right after the symbol table (`PointerToSymbolTable + NumberOfSymbols * 18`;
    the table itself opens with a 4-byte total-length prefix, and offsets are measured from the very
    start of that prefix). This project's gnullvm builds keep their symbol/string table, so real
    names like `.debug_info` show up this way, not as literal 8-byte fields -- confirmed empirically,
    not from the PE spec alone.
    """
    (e_lfanew,) = struct.unpack_from("<I", data, 0x3C)
    if data[e_lfanew : e_lfanew + 4] != b"PE\0\0":
        return []
    coff_off = e_lfanew + 4
    _machine, num_sections, _ts, symtab_ptr, num_syms = struct.unpack_from(
        "<HHIII", data, coff_off
    )
    strtab_off = symtab_ptr + num_syms * 18 if symtab_ptr and num_syms else None

    def resolve(raw_name: bytes) -> str:
        trimmed = raw_name.rstrip(b"\0")
        if trimmed.startswith(b"/") and strtab_off is not None:
            try:
                str_offset = int(trimmed[1:])
            except ValueError:
                return trimmed.decode("ascii", "replace")
            start = strtab_off + str_offset
            end = data.index(b"\0", start)
            return data[start:end].decode("ascii", "replace")
        return trimmed.decode("ascii", "replace")

    return [
        (resolve(name), rawsize)
        for name, _vaddr, rawsize, _rawptr in _pe_section_headers(data, coff_off, num_sections)
    ]


_PDB_MAGIC = b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0"


def identify_pdb(data: bytes) -> list[FileIdentity]:
    # A real PDB info-stream read needs the MSF superblock + stream directory; out of scope for a
    # project that ships no standalone .pdb files (see the module doc comment) -- left unimplemented
    # rather than half-right.
    return []


def identify(path: Path) -> list[FileIdentity]:
    data = path.read_bytes()
    if len(data) < 16:
        return []
    if data.startswith(_PDB_MAGIC):
        return identify_pdb(data)
    magic32 = struct.unpack_from(">I", data, 0)[0]
    if magic32 in _MACHO_MAGICS or magic32 in (_FAT_MAGIC, _FAT_CIGAM):
        return identify_macho(data)
    if data[:4] == b"\x7fELF":
        return identify_elf(data)
    if data[:2] == b"MZ":
        return identify_pe(data)
    return []


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(f"usage: {argv[0]} <file> [--json]", file=sys.stderr)
        return 2
    path = Path(argv[1])
    as_json = "--json" in argv[2:]
    identities = identify(path)
    if not identities:
        return 1
    if as_json:
        print(json.dumps([{"debug_id": i.debug_id, "file_type": i.file_type} for i in identities]))
    else:
        for i in identities:
            print(i.debug_id)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
