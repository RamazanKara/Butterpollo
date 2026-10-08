//! Direct NVIDIA DRS C ABI. No previous C++ host or helper is linked.
#![warn(clippy::undocumented_unsafe_blocks)]

use anyhow::{Context, Result, bail};
use libloading::Library;
use serde::{Deserialize, Serialize};
use std::{ffi::c_void, ptr};
type Handle = *mut c_void;
type Query = unsafe extern "C" fn(u32) -> *mut c_void;
type SessionFn = unsafe extern "C" fn(Handle) -> i32;
type Set = unsafe extern "C" fn(Handle, Handle, *mut Setting) -> i32;
type Get = unsafe extern "C" fn(Handle, Handle, u32, *mut Setting) -> i32;
type Delete = unsafe extern "C" fn(Handle, Handle, u32) -> i32;
#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Global,
    Base,
    Application(String),
}
#[repr(C)]
struct Profile {
    version: u32,
    name: [u16; 2048],
    gpu_support: u32,
    predefined: u32,
    applications: u32,
    settings: u32,
}
#[repr(C)]
struct Application {
    version: u32,
    predefined: u32,
    name: [u16; 2048],
    friendly_name: [u16; 2048],
    launcher: [u16; 2048],
}
fn unicode(value: &str) -> Result<[u16; 2048]> {
    let mut result = [0; 2048];
    let encoded: Vec<_> = value.encode_utf16().collect();
    if encoded.len() >= result.len() || encoded.contains(&0) {
        bail!("NVIDIA profile name is too long or contains a NUL");
    }
    result[..encoded.len()].copy_from_slice(&encoded);
    Ok(result)
}
unsafe fn function<T: Copy>(query: Query, ids: &[u32]) -> Result<T> {
    for id in ids {
        // SAFETY: The caller passes nvapi_QueryInterface from the loaded nvapi64.dll; any id is
        // valid.
        let pointer = unsafe { query(*id) };
        if !pointer.is_null() {
            if size_of::<T>() != size_of::<*mut c_void>() {
                bail!("invalid NvAPI function type");
            }
            // SAFETY: The size check above holds, and the caller guarantees `T` is the function
            // pointer type NvAPI documents for this id.
            return Ok(unsafe { std::mem::transmute_copy(&pointer) });
        }
    }
    bail!("NvAPI interface is unavailable")
}
#[repr(C)]
struct Setting {
    version: u32,
    name: [u16; 2048],
    id: u32,
    kind: u32,
    location: u32,
    predefined: u32,
    predefined_valid: u32,
    default_value: [u32; 1025],
    value: [u32; 1025],
}
impl Setting {
    fn new(id: u32) -> Self {
        // Public NVDRS_SETTING_V1, DWORD/BINARY/UTF16 union, pack(4).
        Self {
            version: size_of::<Self>() as u32 | (1 << 16),
            name: [0; 2048],
            id,
            kind: 0,
            location: 0,
            predefined: 0,
            predefined_valid: 0,
            default_value: [0; 1025],
            value: [0; 1025],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Value {
    pub effective: u32,
    pub override_value: Option<u32>,
}
pub struct Drs {
    _library: Library,
    query: Query,
    session: Handle,
    profile: Handle,
    location: u32,
    set: Set,
    get: Get,
    delete: Delete,
    destroy: SessionFn,
    unload: unsafe extern "C" fn() -> i32,
}
#[derive(Debug)]
pub struct NvError {
    pub code: i32,
    name: String,
}
impl std::fmt::Display for NvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NvAPI {} failed ({})", self.name, self.code)
    }
}
impl std::error::Error for NvError {}
fn check(code: i32, name: &str) -> Result<()> {
    if code != 0 {
        return Err(NvError {
            code,
            name: name.into(),
        }
        .into());
    }
    Ok(())
}
impl Drs {
    pub fn open() -> Result<Self> {
        Self::open_scope(&Scope::Global, false)
    }
    /// Opening a profile never saves changes. Creation, when requested, is part
    /// of the caller's unsaved DRS transaction until its recovery journal exists.
    pub fn open_scope(scope: &Scope, create_profile: bool) -> Result<Self> {
        let path = std::env::var_os("SystemRoot").context("Windows directory missing")?;
        let library =
            // SAFETY: Loads the NVIDIA driver's nvapi64.dll by its full System32 path, so only the
            // driver's own initialisers run.
            unsafe { Library::new(std::path::PathBuf::from(path).join("System32/nvapi64.dll"))? };
        // SAFETY: nvapi_QueryInterface has the Query signature, and `library` is kept in Drs for as
        // long as the pointer is used.
        let query: Query = unsafe {
            *library
                .get(b"nvapi_QueryInterface\0")
                .or_else(|_| library.get(b"NvAPI_QueryInterface\0"))?
        };
        // SAFETY: Each id is resolved to its documented NvAPI function type, the structs are the
        // repr(C) NVDRS v1 layouts, and every pointer passed outlives its call.
        unsafe {
            let init: unsafe extern "C" fn() -> i32 = function(query, &[0x0150e828])?;
            let unload = function(query, &[0xd22bdd7e])?;
            let create: unsafe extern "C" fn(*mut Handle) -> i32 = function(query, &[0x0694d52e])?;
            let destroy = function(query, &[0xdad9cff8])?;
            let set = function(query, &[0x577dd202])?;
            let get = function(query, &[0x73bf8338])?;
            let delete = function(query, &[0xe4a26362])?;
            check(init(), "Initialize")?;
            let mut this = Self {
                _library: library,
                query,
                session: ptr::null_mut(),
                profile: ptr::null_mut(),
                location: 0,
                set,
                get,
                delete,
                destroy,
                unload,
            };
            check(create(&mut this.session), "CreateSession")?;
            let load: SessionFn = function(query, &[0x375dbd6b])?;
            check(load(this.session), "LoadSettings")?;
            match scope {
                Scope::Application(path) => {
                    let mut application = Application {
                        version: size_of::<Application>() as u32 | (1 << 16),
                        predefined: 0,
                        name: unicode(path)?,
                        friendly_name: unicode("Butterpollo Rust")?,
                        launcher: [0; 2048],
                    };
                    let find: unsafe extern "C" fn(
                        Handle,
                        *const u16,
                        *mut Handle,
                        *mut Application,
                    ) -> i32 = function(query, &[0xeee566b2])?;
                    let code = find(
                        this.session,
                        application.name.as_ptr(),
                        &mut this.profile,
                        &mut application,
                    );
                    if code != 0 {
                        if code != -166 || !create_profile {
                            check(code, "FindApplicationByName")?;
                        }
                        let name = unicode("Butterpollo Rust")?;
                        let find: unsafe extern "C" fn(Handle, *const u16, *mut Handle) -> i32 =
                            function(query, &[0x7e4a9a0b])?;
                        let code = find(this.session, name.as_ptr(), &mut this.profile);
                        if code == -163 {
                            let mut profile = Profile {
                                version: size_of::<Profile>() as u32 | (1 << 16),
                                name,
                                gpu_support: 0,
                                predefined: 0,
                                applications: 0,
                                settings: 0,
                            };
                            let create: unsafe extern "C" fn(
                                Handle,
                                *mut Profile,
                                *mut Handle,
                            ) -> i32 = function(query, &[0xcc176068])?;
                            check(
                                create(this.session, &mut profile, &mut this.profile),
                                "CreateProfile",
                            )?;
                        } else {
                            check(code, "FindProfileByName")?;
                        }
                        // FindApplicationByName may have filled the structure.
                        application.name = unicode(path)?;
                        application.predefined = 0;
                        let create: unsafe extern "C" fn(Handle, Handle, *mut Application) -> i32 =
                            function(query, &[0x4347a9de])?;
                        check(
                            create(this.session, this.profile, &mut application),
                            "CreateApplication",
                        )?;
                    }
                }
                Scope::Global | Scope::Base => {
                    if matches!(scope, Scope::Global) {
                        let global: unsafe extern "C" fn(Handle, *mut Handle) -> i32 =
                            function(query, &[0x617bff9f])?;
                        if global(this.session, &mut this.profile) == 0 && !this.profile.is_null() {
                            this.location = 1;
                        }
                    }
                    if this.location != 1 {
                        let base: unsafe extern "C" fn(Handle, *mut Handle) -> i32 =
                            function(query, &[0xda8466a0])?;
                        check(base(this.session, &mut this.profile), "GetBaseProfile")?;
                        this.location = 2;
                    }
                }
            }
            if this.profile.is_null() {
                bail!("NvAPI returned an empty profile");
            }
            Ok(this)
        }
    }
    pub fn read(&self, id: u32) -> Result<Value> {
        let mut setting = Setting::new(id);
        // SAFETY: `get` is NvAPI_DRS_GetSetting from the library Drs keeps loaded, with the open
        // session and profile; `setting` is a versioned NVDRS_SETTING that outlives the call.
        let code = unsafe { (self.get)(self.session, self.profile, id, &mut setting) };
        if code == -160 {
            return Ok(Value {
                effective: 0,
                override_value: None,
            });
        }
        check(code, "GetSetting")?;
        if setting.kind != 0 {
            bail!("NvAPI returned a non-DWORD setting");
        }
        let owned =
            setting.predefined == 0 && (setting.location == 0 || setting.location == self.location);
        Ok(Value {
            effective: setting.value[0],
            override_value: owned.then_some(setting.value[0]),
        })
    }
    /// Driver profile tuning, including predefined values. Application queries
    /// exclude inherited global values so app/global precedence stays explicit.
    pub fn profile_dword(&self, id: u32) -> Result<Option<u32>> {
        let mut setting = Setting::new(id);
        // SAFETY: `get` is NvAPI_DRS_GetSetting from the library Drs keeps loaded, with the open
        // session and profile; `setting` is a versioned NVDRS_SETTING that outlives the call.
        let code = unsafe { (self.get)(self.session, self.profile, id, &mut setting) };
        if code == -160 {
            return Ok(None);
        }
        check(code, "GetSetting")?;
        let visible =
            setting.location == 0 || setting.location == if self.location == 0 { 2 } else { 1 };
        Ok((setting.kind == 0 && visible).then_some(setting.value[0]))
    }
    pub fn write(&self, id: u32, value: Option<u32>) -> Result<()> {
        if let Some(value) = value {
            let mut setting = Setting::new(id);
            setting.value[0] = value;
            check(
                // SAFETY: NvAPI_DRS_SetSetting with the open session and profile, and a live
                // `setting`.
                unsafe { (self.set)(self.session, self.profile, &mut setting) },
                "SetSetting",
            )
        } else {
            // SAFETY: NvAPI_DRS_DeleteProfileSetting with the open session and profile; no
            // pointers.
            let code = unsafe { (self.delete)(self.session, self.profile, id) };
            if code == -160 {
                Ok(())
            } else {
                check(code, "DeleteProfileSetting")
            }
        }
    }
    pub fn save(&self) -> Result<()> {
        // SAFETY: `query` is nvapi_QueryInterface from the library Drs keeps loaded.
        let f = unsafe { (self.query)(0xfcbc7e14) };
        if f.is_null() {
            bail!("NvAPI SaveSettings is unavailable");
        }
        // SAFETY: `f` is non-null and 0xfcbc7e14 is NvAPI_DRS_SaveSettings, with the SessionFn
        // signature.
        let save: SessionFn = unsafe { std::mem::transmute(f) };
        // SAFETY: `save` takes the session this Drs opened and still owns.
        check(unsafe { save(self.session) }, "SaveSettings")
    }
    pub fn version(&self) -> Result<u32> {
        // SAFETY: `query` is nvapi_QueryInterface from the library Drs keeps loaded.
        let f = unsafe { (self.query)(0x2926aaad) };
        if f.is_null() {
            return Ok(0);
        }
        // SAFETY: `f` is non-null and 0x2926aaad is NvAPI_SYS_GetDriverAndBranchVersion, which has
        // this signature.
        let version: unsafe extern "C" fn(*mut u32, *mut u8) -> i32 =
            unsafe { std::mem::transmute(f) };
        let mut result = 0;
        let mut branch = [0; 64];
        check(
            // SAFETY: `result` and the 64-byte `branch` (an NvAPI_ShortString) outlive the call.
            unsafe { version(&mut result, branch.as_mut_ptr()) },
            "GetDriverAndBranchVersion",
        )?;
        Ok(result)
    }
}
impl Drop for Drs {
    fn drop(&mut self) {
        // SAFETY: `session` was created by this Drs and is destroyed once; NvAPI is unloaded before
        // the `_library` field is dropped.
        unsafe {
            if !self.session.is_null() {
                (self.destroy)(self.session);
            }
            (self.unload)();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drs_v1_abi_matches_retained_nvidia_sdk() {
        assert_eq!(size_of::<Setting>(), 12320);
        assert_eq!(std::mem::offset_of!(Setting, id), 4100);
        assert_eq!(std::mem::offset_of!(Setting, value), 8220);
        assert_eq!(Setting::new(0).version, 0x13020);
        assert_eq!(size_of::<Profile>(), 4116);
        assert_eq!(std::mem::offset_of!(Profile, gpu_support), 4100);
        assert_eq!(size_of::<Application>(), 12296);
        assert_eq!(std::mem::offset_of!(Application, name), 8);
        assert_eq!(unicode("host-🦀").unwrap()[5], 0xd83e);
        assert!(unicode("bad\0name").is_err());
        assert!(unicode(&"a".repeat(2048)).is_err());
    }
}
