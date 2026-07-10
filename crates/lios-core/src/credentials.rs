use std::fs;
use std::path::Path;

use crate::Result;

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    pub fn protect_to_file(token: &str, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut input = token.as_bytes().to_vec();
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: input.len() as u32,
            pbData: input.as_mut_ptr(),
        };
        let mut out_blob = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&in_blob, PWSTR::null(), None, None, None, 0, &mut out_blob)
                .map_err(|_| crate::LiosError::Crypto)?;
            let protected =
                std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
            let _ = LocalFree(HLOCAL(out_blob.pbData.cast()));
            fs::write(path, protected)?;
        }
        Ok(())
    }

    pub fn unprotect_from_file(path: &Path) -> Result<String> {
        let mut bytes = fs::read(path)?;
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_mut_ptr(),
        };
        let mut out_blob = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&in_blob, None, None, None, None, 0, &mut out_blob)
                .map_err(|_| crate::LiosError::Crypto)?;
            let plain =
                std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
            let _ = LocalFree(HLOCAL(out_blob.pbData.cast()));
            String::from_utf8(plain).map_err(|_| crate::LiosError::InvalidKeyFile)
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn protect_to_file(token: &str, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, token.as_bytes())?;
        Ok(())
    }

    pub fn unprotect_from_file(path: &Path) -> Result<String> {
        Ok(fs::read_to_string(path)?)
    }
}

pub use platform::{protect_to_file, unprotect_from_file};
