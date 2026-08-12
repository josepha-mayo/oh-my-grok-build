//! Helpers for reading the user SID out of a Windows TOKEN_USER buffer.

use anyhow::{Context, Result, bail};

/// Read the PSID pointer from the first bytes of a `TOKEN_USER` buffer.
///
/// `TOKEN_USER` begins with a `SID_AND_ATTRIBUTES` whose first field is the
/// PSID. Because the buffer is a `Vec<u8>` and may be under-aligned, we use
/// `read_unaligned` instead of taking a reference.
pub fn sid_ptr(buf: &[u8]) -> Result<windows::Win32::Security::PSID> {
    if buf.len() < std::mem::size_of::<windows::Win32::Security::PSID>() {
        bail!("TOKEN_USER buffer is too short to contain a SID pointer");
    }
    // SAFETY: We are copying the bytes at the start of the buffer into a PSID
    // value. No reference to the under-aligned u8 buffer is created.
    let sid =
        unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const windows::Win32::Security::PSID) };
    if sid.0.is_null() {
        bail!("TOKEN_USER contains a null SID pointer");
    }
    Ok(sid)
}

/// Return the raw bytes of a SID for constant-time comparison.
///
/// A SID is at least 8 bytes: revision, sub-authority count, 6-byte authority,
/// then `count * 4` bytes of sub-authorities. The token buffer is supplied so
/// an untrusted or malformed pointer can never be read out of bounds.
pub fn sid_bytes(token_user: &[u8], sid: windows::Win32::Security::PSID) -> Result<Vec<u8>> {
    let start = sid.0 as usize;
    let buffer_start = token_user.as_ptr() as usize;
    let buffer_end = buffer_start
        .checked_add(token_user.len())
        .context("TOKEN_USER buffer length overflow")?;
    let header_end = start
        .checked_add(8)
        .context("SID pointer arithmetic overflow")?;
    if sid.0.is_null() || start < buffer_start || header_end > buffer_end {
        bail!("SID pointer is outside its TOKEN_USER buffer");
    }

    let offset = start - buffer_start;
    let count = token_user[offset + 1] as usize;
    let len = 8usize
        .checked_add(
            count
                .checked_mul(4)
                .context("SID sub-authority count overflow")?,
        )
        .context("SID length overflow")?;
    let sid_end = offset.checked_add(len).context("SID length overflow")?;
    if sid_end > token_user.len() {
        bail!("SID extends beyond its TOKEN_USER buffer");
    }
    Ok(token_user[offset..sid_end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Security::PSID;

    #[test]
    fn sid_bytes_rejects_a_pointer_outside_the_token_buffer() {
        let token_user = [0u8; 8];
        assert!(sid_bytes(&token_user, PSID(std::ptr::dangling_mut())).is_err());
    }
}
