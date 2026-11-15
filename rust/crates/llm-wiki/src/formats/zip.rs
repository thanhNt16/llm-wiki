//! Minimal zip reader for office containers (docx/pptx/xlsx/odt/ods).
//!
//! Read-only central-directory parser — intentionally NOT a general zip
//! library. Only stored + deflated members are handled, archive-local
//! headers are trusted for data offsets only (sizes come from the central
//! directory), and crow's ingest limits are enforced here (spec §8):
//!
//! - `MAX_PART` per-member uncompressed cap (64 MB),
//! - `MAX_MEMBERS` entry-count cap,
//! - `MAX_RATIO` total-uncompressed : archive-size expansion cap,
//! - encrypted / ZIP64 members are refused rather than parsed.

use std::io::Read;

use flate2::read::DeflateDecoder;

use crate::err::EngineError;

/// Per-member uncompressed cap (spec §8).
pub const MAX_PART: u64 = 64 * 1024 * 1024;
/// Central-directory entry cap — keeps pathological archives cheap.
pub const MAX_MEMBERS: u64 = 10_000;
/// Total uncompressed : archive-size expansion cap (zip-bomb guard).
pub const MAX_RATIO: u64 = 100;

fn corrupt() -> EngineError {
    EngineError::Txn("corrupt zip archive".into())
}

fn le16(b: &[u8]) -> u64 {
    u16::from_le_bytes([b[0], b[1]]) as u64
}

fn le32(b: &[u8]) -> u64 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64
}

#[derive(Debug, Clone)]
pub struct Member {
    pub name: String,
    flags: u16,
    method: u16,
    comp_size: u64,
    unc_size: u64,
    local_off: u64,
}

/// Central-directory view over borrowed archive bytes; `open` returns
/// decompressed member content (capped at `MAX_PART`).
pub struct ZipArchive<'a> {
    data: &'a [u8],
    members: Vec<Member>,
}

impl<'a> ZipArchive<'a> {
    pub fn open(data: &'a [u8]) -> Result<Self, EngineError> {
        if data.len() < 22 {
            return Err(corrupt());
        }
        // EOCD: scan back from EOF within the max comment window; the tail
        // byte `i + 22` must land exactly on EOF or the record is bogus.
        let scan_start = data.len().saturating_sub(22 + 65536);
        let mut eocd = None;
        for i in (scan_start..=data.len() - 22).rev() {
            if data[i..i + 4] == [0x50, 0x4B, 0x05, 0x06] && i + 22 + le16(&data[i + 20..]) as usize == data.len() {
                eocd = Some(i);
                break;
            }
        }
        let i = eocd.ok_or_else(corrupt)?;
        let entries = le16(&data[i + 10..]);
        let cd_size = le32(&data[i + 12..]);
        let cd_off = le32(&data[i + 16..]);
        if entries >= 0xFFFF || cd_size >= 0xFFFF_FFFF || cd_off >= 0xFFFF_FFFF {
            return Err(EngineError::Txn("zip64 archives not supported".into()));
        }
        if entries > MAX_MEMBERS {
            return Err(EngineError::Txn(
                "archive exceeds limits: too many members".into(),
            ));
        }
        if cd_off + cd_size > data.len() as u64 {
            return Err(corrupt());
        }
        let mut members = Vec::with_capacity(entries as usize);
        let mut off = cd_off as usize;
        let mut total_unc: u64 = 0;
        for _ in 0..entries {
            if off + 46 > data.len() || data[off..off + 4] != [0x50, 0x4B, 0x01, 0x02] {
                return Err(corrupt());
            }
            let flags = le16(&data[off + 8..]) as u16;
            let method = le16(&data[off + 10..]) as u16;
            let comp_size = le32(&data[off + 20..]);
            let unc_size = le32(&data[off + 24..]);
            let nlen = le16(&data[off + 28..]) as usize;
            let elen = le16(&data[off + 30..]) as usize;
            let clen = le16(&data[off + 32..]) as usize;
            let local_off = le32(&data[off + 42..]);
            if comp_size >= 0xFFFF_FFFF || unc_size >= 0xFFFF_FFFF || local_off >= 0xFFFF_FFFF {
                return Err(EngineError::Txn("zip64 archives not supported".into()));
            }
            if unc_size > MAX_PART {
                return Err(EngineError::Txn(
                    "archive exceeds limits: member too large".into(),
                ));
            }
            let name_off = off + 46;
            if name_off + nlen > data.len() {
                return Err(corrupt());
            }
            total_unc += unc_size;
            let name = String::from_utf8_lossy(&data[name_off..name_off + nlen]).into_owned();
            members.push(Member {
                name,
                flags,
                method,
                comp_size,
                unc_size,
                local_off,
            });
            off = name_off + nlen + elen + clen;
        }
        if total_unc > (data.len() as u64).saturating_mul(MAX_RATIO) {
            return Err(EngineError::Txn(
                "archive exceeds limits: expansion ratio".into(),
            ));
        }
        Ok(Self { data, members })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(|m| m.name.as_str())
    }

    pub fn has(&self, name: &str) -> bool {
        self.members.iter().any(|m| m.name == name)
    }

    /// Decompress member `name`; errors on missing member, encrypted flag,
    /// unsupported method, size lies, or a stream that exceeds `MAX_PART`.
    pub fn member(&self, name: &str) -> Result<Vec<u8>, EngineError> {
        let m = self
            .members
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| EngineError::Txn(format!("missing archive member {name}")))?;
        if m.flags & 0x0001 != 0 {
            return Err(EngineError::Txn(format!(
                "encrypted zip member {name}"
            )));
        }
        let lo = m.local_off as usize;
        if lo + 30 > self.data.len() || self.data[lo..lo + 4] != [0x50, 0x4B, 0x03, 0x04] {
            return Err(corrupt());
        }
        let l_nlen = le16(&self.data[lo + 26..]) as usize;
        let l_elen = le16(&self.data[lo + 28..]) as usize;
        let dstart = lo + 30 + l_nlen + l_elen;
        let dend = dstart + m.comp_size as usize;
        if dend > self.data.len() {
            return Err(corrupt());
        }
        let comp = &self.data[dstart..dend];
        match m.method {
            0 => {
                if comp.len() as u64 != m.unc_size {
                    return Err(corrupt());
                }
                Ok(comp.to_vec())
            }
            8 => {
                // Read at most MAX_PART+1 bytes regardless of the claimed
                // size — a lying header can't inflate past the cap.
                let mut out = Vec::with_capacity(m.unc_size.min(MAX_PART) as usize);
                DeflateDecoder::new(comp)
                    .take(MAX_PART + 1)
                    .read_to_end(&mut out)
                    .map_err(|e| EngineError::Txn(format!("zip member {name}: {e}")))?;
                if out.len() as u64 > MAX_PART {
                    return Err(EngineError::Txn(
                        "archive exceeds limits: member too large".into(),
                    ));
                }
                if out.len() as u64 != m.unc_size {
                    return Err(corrupt());
                }
                Ok(out)
            }
            other => Err(EngineError::Txn(format!(
                "unsupported zip compression method {other} for {name}"
            ))),
        }
    }
}
