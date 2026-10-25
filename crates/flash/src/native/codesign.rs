//! Ad-hoc code signatures for Mach-O executables.
//!
//! Apple Silicon refuses to run an arm64 executable that is not signed, but an *ad-hoc*
//! signature needs no certificate or key: it is a SuperBlob holding one CodeDirectory, which lists
//! the SHA-256 hash of every 4 KiB page of the file (the signature region itself excluded). The
//! layout is the one `ld64`/`lld` produce for "linker-signed" binaries; fields are big-endian.

use sha2::{Digest, Sha256};

const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xFADE_0CC0;
const CSMAGIC_CODEDIRECTORY: u32 = 0xFADE_0C02;
const CSSLOT_CODEDIRECTORY: u32 = 0;
const CS_ADHOC: u32 = 0x2;
const CS_LINKER_SIGNED: u32 = 0x2_0000;
const CS_SUPPORTSEXECSEG: u32 = 0x2_0400;
const CS_HASHTYPE_SHA256: u8 = 2;
const CS_EXECSEG_MAIN_BINARY: u64 = 1;
const PAGE_SHIFT: u8 = 12;
const PAGE: usize = 1 << PAGE_SHIFT;
const SUPERBLOB_HEADER: usize = 12 + 8; // magic, length, count, one index entry
const CODEDIRECTORY_FIXED: usize = 88;
const SHA256_LEN: usize = 32;

fn ident_field_len(ident: &str) -> usize {
    (ident.len() + 1).div_ceil(16) * 16
}

/// Offset of the CodeDirectory inside the SuperBlob (8-byte aligned, as the linkers lay it out).
const CD_OFFSET: usize = 24;

/// Size in bytes of the signature for a file whose signed prefix has `code_limit` bytes.
pub fn signature_size(code_limit: usize, ident: &str) -> usize {
    CD_OFFSET + CODEDIRECTORY_FIXED + ident_field_len(ident) + SHA256_LEN * code_limit.div_ceil(PAGE)
}

/// Build the signature for `code` (the whole file up to, not including, the signature).
/// `text_size` is the file size of the `__TEXT` segment (which starts at file offset 0).
pub fn ad_hoc(code: &[u8], text_size: u64, ident: &str) -> Vec<u8> {
    let pages = code.len().div_ceil(PAGE);
    let hash_offset = CODEDIRECTORY_FIXED + ident_field_len(ident);
    let cd_len = hash_offset + SHA256_LEN * pages;
    let mut cd: Vec<u8> = Vec::with_capacity(cd_len);
    let be32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_be_bytes());
    let be64 = |v: &mut Vec<u8>, x: u64| v.extend_from_slice(&x.to_be_bytes());
    be32(&mut cd, CSMAGIC_CODEDIRECTORY);
    be32(&mut cd, cd_len as u32);
    be32(&mut cd, CS_SUPPORTSEXECSEG);
    be32(&mut cd, CS_ADHOC | CS_LINKER_SIGNED);
    be32(&mut cd, hash_offset as u32);
    be32(&mut cd, CODEDIRECTORY_FIXED as u32); // identOffset
    be32(&mut cd, 0); // nSpecialSlots
    be32(&mut cd, pages as u32);
    be32(&mut cd, code.len() as u32); // codeLimit
    cd.push(SHA256_LEN as u8);
    cd.push(CS_HASHTYPE_SHA256);
    cd.push(0); // platform
    cd.push(PAGE_SHIFT);
    be32(&mut cd, 0); // spare2
    be32(&mut cd, 0); // scatterOffset
    be32(&mut cd, 0); // teamOffset
    be32(&mut cd, 0); // spare3
    be64(&mut cd, 0); // codeLimit64
    be64(&mut cd, 0); // execSegBase: __TEXT starts at file offset 0
    be64(&mut cd, text_size); // execSegLimit
    be64(&mut cd, CS_EXECSEG_MAIN_BINARY);
    debug_assert_eq!(cd.len(), CODEDIRECTORY_FIXED);
    cd.extend_from_slice(ident.as_bytes());
    cd.resize(hash_offset, 0);
    for page in code.chunks(PAGE) {
        cd.extend_from_slice(&Sha256::digest(page));
    }
    let mut out: Vec<u8> = Vec::with_capacity(CD_OFFSET + cd_len);
    be32(&mut out, CSMAGIC_EMBEDDED_SIGNATURE);
    be32(&mut out, (CD_OFFSET + cd_len) as u32);
    be32(&mut out, 1);
    be32(&mut out, CSSLOT_CODEDIRECTORY);
    be32(&mut out, CD_OFFSET as u32);
    out.resize(CD_OFFSET, 0);
    out.extend_from_slice(&cd);
    debug_assert_eq!(out.len(), signature_size(code.len(), ident));
    debug_assert!(SUPERBLOB_HEADER <= CD_OFFSET);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signature `lld` produced for a tiny arm64 executable (checked in as a hex dump of the
    /// file's first 16512 bytes' hash-relevant parts would be large; instead the test rebuilds a
    /// signature from scratch for synthetic data and checks the structure).
    #[test]
    fn structure_is_consistent() {
        let code: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let sig = ad_hoc(&code, 4096, "prog");
        assert_eq!(sig.len(), signature_size(code.len(), "prog"));
        assert_eq!(&sig[..4], &CSMAGIC_EMBEDDED_SIGNATURE.to_be_bytes());
        assert_eq!(u32::from_be_bytes(sig[4..8].try_into().unwrap()) as usize, sig.len());
        let cd = &sig[CD_OFFSET..];
        assert_eq!(&cd[..4], &CSMAGIC_CODEDIRECTORY.to_be_bytes());
        let n_pages = u32::from_be_bytes(cd[28..32].try_into().unwrap());
        assert_eq!(n_pages, 3);
        let hash_off = u32::from_be_bytes(cd[16..20].try_into().unwrap()) as usize;
        // the second page's hash
        let h = Sha256::digest(&code[4096..8192]);
        assert_eq!(&cd[hash_off + 32..hash_off + 64], h.as_slice());
        // the partial last page is hashed as it is
        let h = Sha256::digest(&code[8192..]);
        assert_eq!(&cd[hash_off + 64..hash_off + 96], h.as_slice());
    }
}
