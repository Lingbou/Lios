#![cfg(windows)]

use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::ptr;

use lios_core::crypto::KeyFile;
use tempfile::tempdir;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows::Win32::Security::{
    AclSizeInformation, CreateWellKnownSid, EqualSid, GetAce, GetAclInformation,
    GetSecurityDescriptorControl, WinBuiltinAdministratorsSid, WinBuiltinUsersSid,
    WinLocalSystemSid, WinWorldSid, ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION,
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_MAX_SID_SIZE, SE_DACL_PROTECTED, WELL_KNOWN_SID_TYPE,
};
use windows::Win32::Storage::FileSystem::{FILE_ALL_ACCESS, FILE_GENERIC_READ};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(HLOCAL(self.0 .0));
        }
    }
}

fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> Vec<u8> {
    let mut sid = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut sid_len = sid.len() as u32;
    unsafe {
        CreateWellKnownSid(
            kind,
            PSID::default(),
            PSID(sid.as_mut_ptr().cast()),
            &mut sid_len,
        )
        .unwrap();
    }
    sid.truncate(sid_len as usize);
    sid
}

fn sid_from_bytes(bytes: &[u8]) -> PSID {
    PSID(bytes.as_ptr().cast_mut().cast())
}

#[test]
fn generated_key_has_protected_restrictive_dacl() {
    let tmp = tempdir().unwrap();
    let key_path = tmp.path().join("recovery.key");
    KeyFile::generate_to_path(&key_path).unwrap();

    let wide_path = key_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut owner = PSID::default();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide_path.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            &mut descriptor,
        )
    };
    assert_eq!(status.0, 0, "GetNamedSecurityInfoW failed: {status:?}");
    let _descriptor = LocalSecurityDescriptor(descriptor);

    let mut control = 0u16;
    let mut revision = 0u32;
    unsafe {
        GetSecurityDescriptorControl(descriptor, &mut control, &mut revision).unwrap();
    }
    assert_ne!(control & SE_DACL_PROTECTED.0, 0, "DACL must be protected");
    assert!(!dacl.is_null(), "key file must have a DACL");

    let mut acl_info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            dacl,
            (&mut acl_info as *mut ACL_SIZE_INFORMATION).cast::<c_void>(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
        .unwrap();
    }

    let system = well_known_sid(WinLocalSystemSid);
    let administrators = well_known_sid(WinBuiltinAdministratorsSid);
    let everyone = well_known_sid(WinWorldSid);
    let users = well_known_sid(WinBuiltinUsersSid);
    let mut owner_full_control = false;
    let mut system_full_control = false;
    let mut administrators_full_control = false;

    for index in 0..acl_info.AceCount {
        let mut ace_ptr = ptr::null_mut();
        unsafe {
            GetAce(dacl, index, &mut ace_ptr).unwrap();
        }
        let ace = unsafe { &*ace_ptr.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Header.AceType != ACCESS_ALLOWED_ACE_TYPE {
            continue;
        }
        let ace_sid = PSID((&ace.SidStart as *const u32).cast_mut().cast());
        let grants_full_control = ace.Mask & FILE_ALL_ACCESS.0 == FILE_ALL_ACCESS.0;
        owner_full_control |= unsafe { EqualSid(ace_sid, owner).is_ok() } && grants_full_control;
        system_full_control |=
            unsafe { EqualSid(ace_sid, sid_from_bytes(&system)).is_ok() } && grants_full_control;
        administrators_full_control |=
            unsafe { EqualSid(ace_sid, sid_from_bytes(&administrators)).is_ok() }
                && grants_full_control;

        if ace.Mask & FILE_GENERIC_READ.0 != 0 {
            assert!(
                unsafe { EqualSid(ace_sid, sid_from_bytes(&everyone)).is_err() },
                "Everyone must not have a readable allow ACE"
            );
            assert!(
                unsafe { EqualSid(ace_sid, sid_from_bytes(&users)).is_err() },
                "Builtin Users must not have a readable allow ACE"
            );
        }
    }

    assert!(owner_full_control, "owner must retain full control");
    assert!(system_full_control, "SYSTEM must retain full control");
    assert!(
        administrators_full_control,
        "Administrators must retain full control"
    );
}
