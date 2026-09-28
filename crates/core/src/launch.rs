use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
};

/// Metadata extracted from a validated Linux x86_64 executable.
pub struct ElfInfo {
    /// Absolute dynamic loader path, or `None` for a static executable.
    pub interpreter: Option<String>,
}

/// Validates a Linux x86_64 ELF executable and returns its launch metadata.
pub fn inspect_elf(file: &mut File) -> io::Result<ElfInfo> {
    let invalid = || io::Error::other("Expected a Linux x86_64 executable ELF");
    file.rewind()?;
    let length = file.metadata()?.len();
    let mut header = [0u8; 64];
    file.read_exact(&mut header)?;
    let u16_at = |i| u16::from_le_bytes(header[i..i + 2].try_into().unwrap());
    let u64_at = |i| u64::from_le_bytes(header[i..i + 8].try_into().unwrap());
    // Only executable x86_64 ELF64 files with the expected header layout can be launched.
    if &header[..7] != b"\x7fELF\x02\x01\x01"
        || !matches!(header[7], 0 | 3)
        || !matches!(u16_at(16), 2 | 3)
        || u16_at(18) != 62
        || header[20..24] != [1, 0, 0, 0]
        || u16_at(52) != 64
        || u16_at(54) != 56
        || u16_at(56) == 0
        || u16_at(56) > 4096
    {
        return Err(invalid());
    }
    let phoff = u64_at(32);
    let entry = u64_at(24);
    // Reject an out-of-bounds program header table before seeking to any entry.
    if phoff
        .checked_add(u16_at(56) as u64 * 56)
        .is_none_or(|end| end > length)
    {
        return Err(invalid());
    }
    let mut executable_entry = false;
    let mut interpreter = None;
    for index in 0..u16_at(56) as u64 {
        file.seek(SeekFrom::Start(phoff + index * 56))?;
        let mut ph = [0u8; 56];
        file.read_exact(&mut ph)?;
        let kind = u32::from_le_bytes(ph[..4].try_into().unwrap());
        let flags = u32::from_le_bytes(ph[4..8].try_into().unwrap());
        let at = |i| u64::from_le_bytes(ph[i..i + 8].try_into().unwrap());
        let offset = at(8);
        let virtual_address = at(16);
        let file_size = at(32);
        let memory_size = at(40);
        if offset.checked_add(file_size).is_none_or(|end| end > length) {
            return Err(invalid());
        }
        if kind == 1 {
            if file_size > memory_size {
                return Err(invalid());
            }
            // An entry point outside an executable file-backed segment cannot be launched.
            executable_entry |= flags & 1 != 0
                && entry >= virtual_address
                && virtual_address
                    .checked_add(file_size)
                    .is_some_and(|end| entry < end);
        }
        if kind == 3 {
            // A dynamic executable must declare one bounded, absolute loader path.
            if interpreter.is_some() || !(2..=4096).contains(&file_size) {
                return Err(invalid());
            }
            let mut bytes = vec![0; file_size as usize];
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut bytes)?;
            if bytes.pop() != Some(0) || bytes.contains(&0) {
                return Err(invalid());
            }
            let value = String::from_utf8(bytes).map_err(|_| invalid())?;
            if !value.starts_with('/') || value.chars().any(char::is_control) {
                return Err(invalid());
            }
            interpreter = Some(value);
        }
    }
    if !executable_entry {
        return Err(invalid());
    }
    Ok(ElfInfo { interpreter })
}

#[cfg(test)]
/// Fixtures and checks for executable inspection.
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    /// Builds a minimal valid executable for parser tests.
    pub fn elf() -> Vec<u8> {
        let mut bytes = vec![0; 120];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[20] = 1;
        bytes[24..32].copy_from_slice(&0x400078u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56] = 1;
        bytes[64] = 1;
        bytes[68] = 5;
        bytes[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&121u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&121u64.to_le_bytes());
        bytes.push(0xc3);
        bytes
    }

    // Invalid CPU, truncation, and entry-point layouts must fail ELF inspection.
    #[test]
    fn elf_rejects_wrong_cpu_truncation_and_unmapped_entry_points() {
        let bytes = elf();
        for mode in 0..5 {
            let mut bytes = bytes.clone();
            match mode {
                1 => bytes[18] = 183,
                2 => bytes.truncate(100),
                3 => bytes[24..32].fill(0),
                4 => bytes[68] = 4,
                _ => {}
            }
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&bytes).unwrap();
            assert_eq!(inspect_elf(&mut file).is_ok(), mode == 0);
        }
    }
}
