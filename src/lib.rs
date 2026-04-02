#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::collections::{BTreeSet, HashSet};
use std::ffi::{CStr, CString, c_void};
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;
use std::sync::{
    Arc,
    atomic::{AtomicPtr, Ordering},
};
use std::time::Duration;
use std::{fmt, u64};

use eyre::Result;
//use ffi_convert::RawPointerConverter;
//use ffi_convert::{CReprOf, CStringArray};
use fs_err::File;
use itertools::Itertools;

use glib_sys::GVariant;
use glib_sys::g_variant_get;
use glib_sys::{G_LOG_LEVEL_DEBUG, g_log};

mod packagekit;
use moss::{
    Installation, Package, Provider, Repository,
    client::{Client, ProgressStage, fetch::DownloadCallback},
    environment,
    package::Flags,
    package::{self, Id},
    registry::transaction,
    repository::{self, Priority},
    runtime,
};
use packagekit::{
    _GVariant, GError, GKeyFile, PK_PACKAGE_ID_ARCH, PK_PACKAGE_ID_DATA, PK_PACKAGE_ID_NAME,
    PK_PACKAGE_ID_VERSION, PkBackend, PkBackendJob, PkBitfield,
    PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION, PkErrorEnum_PK_ERROR_ENUM_INTERNAL_ERROR,
    PkErrorEnum_PK_ERROR_ENUM_NOT_SUPPORTED, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_ALREADY_INSTALLED,
    PkErrorEnum_PK_ERROR_ENUM_PACKAGE_DOWNLOAD_FAILED,
    PkErrorEnum_PK_ERROR_ENUM_PACKAGE_ID_INVALID, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND,
    PkErrorEnum_PK_ERROR_ENUM_REPO_CONFIGURATION_ERROR, PkErrorEnum_PK_ERROR_ENUM_REPO_NOT_FOUND,
    PkFilterEnum_PK_FILTER_ENUM_INSTALLED, PkFilterEnum_PK_FILTER_ENUM_NEWEST,
    PkFilterEnum_PK_FILTER_ENUM_NOT_INSTALLED, PkFilterEnum_PK_FILTER_ENUM_NOT_NEWEST,
    PkGroupEnum_PK_GROUP_ENUM_UNKNOWN, PkInfoEnum_PK_INFO_ENUM_AVAILABLE,
    PkInfoEnum_PK_INFO_ENUM_DOWNLOADING, PkInfoEnum_PK_INFO_ENUM_INSTALL,
    PkInfoEnum_PK_INFO_ENUM_INSTALLED, PkInfoEnum_PK_INFO_ENUM_NORMAL,
    PkInfoEnum_PK_INFO_ENUM_REMOVE, PkInfoEnum_PK_INFO_ENUM_UPDATING,
    PkRestartEnum_PK_RESTART_ENUM_NONE, PkStatusEnum_PK_STATUS_ENUM_DEP_RESOLVE,
    PkStatusEnum_PK_STATUS_ENUM_DOWNLOAD, PkStatusEnum_PK_STATUS_ENUM_INSTALL,
    PkStatusEnum_PK_STATUS_ENUM_REFRESH_CACHE, PkStatusEnum_PK_STATUS_ENUM_REMOVE,
    PkStatusEnum_PK_STATUS_ENUM_UPDATE,
    PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_ONLY_DOWNLOAD,
    PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_SIMULATE,
    PkUpdateStateEnum_PK_UPDATE_STATE_ENUM_UNKNOWN, g_ptr_array_add, g_ptr_array_new_full,
    pk_backend_job_details, pk_backend_job_error_code, pk_backend_job_files,
    pk_backend_job_finished, pk_backend_job_package, pk_backend_job_packages,
    pk_backend_job_repo_detail, pk_backend_job_set_item_progress, pk_backend_job_set_percentage,
    pk_backend_job_set_status, pk_backend_job_thread_create, pk_backend_job_update_detail,
    pk_package_id_build, pk_package_id_check, pk_package_id_split, pk_package_new,
    pk_package_set_id, pk_package_set_info, pk_package_set_summary,
};
use stone::{
    StoneDecodedPayload, StonePayloadLayoutFile, StonePayloadMetaPrimitive, StonePayloadMetaTag,
};
use url::Url;
use vfs::tree::BlitFile;

use crate::packagekit::{PkInfoEnum_PK_INFO_ENUM_INSTALLING, PkInfoEnum_PK_INFO_ENUM_REMOVING};

//include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

fn c_char_ptr_to_str(ptr: *const c_char) -> Option<&'static str> {
    if ptr.is_null() {
        return None;
    }

    unsafe {
        let c_str = CStr::from_ptr(ptr);
        match c_str.to_str() {
            Ok(s) => Some(s),
            Err(_) => None, // not valid UTF-8
        }
    }
}

fn c_char_array_to_vec(ptr: *mut *const c_char) -> Vec<String> {
    let mut result = Vec::new();
    if ptr.is_null() {
        return result;
    }

    let mut i = 0;
    loop {
        let p = unsafe { *ptr.add(i) };
        if p.is_null() {
            break;
        }

        let cstr = unsafe { CStr::from_ptr(p) };
        match cstr.to_str() {
            Ok(s) => result.push(s.to_string()),
            Err(_) => (), // skip invalid UTF-8
        }

        i += 1;
    }

    result
}

trait PkErr<T> {
    fn pk_err(self, job: *mut PkBackendJob) -> T;
}

impl<T, E: std::fmt::Display> PkErr<T> for Result<T, E> {
    fn pk_err(self, job: *mut PkBackendJob) -> T {
        match self {
            Ok(val) => val,
            Err(e) => unsafe {
                let msg = e.to_string();
                let c_msg = CString::new(msg.clone())
                    .unwrap_or_else(|_| CString::new("unknown error").unwrap());

                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_INTERNAL_ERROR,
                    c_msg.as_ptr(),
                );
                // we want packagekit to handle this and cleanup naturally
                // TODO: still some testing to do here
                std::thread::sleep(Duration::from_millis(2000));
                panic!("Hopefully packagekit cleans up in time..., error: {msg}")
            },
        }
    }
}

// println will just get eaten, ensure we can print logs to packagekitd --verbose output
pub fn log_debug(args: fmt::Arguments) {
    unsafe {
        let domain = CString::new("PackageKit").unwrap();
        let formatted = format!("{}", args);
        let message = CString::new(formatted).unwrap();
        g_log(domain.as_ptr(), G_LOG_LEVEL_DEBUG, message.as_ptr());
    }
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => ({
        $crate::log_debug(format_args!($($arg)*))
    });
}

// Complex C Macros do not get translated to FFI
#[inline]
fn pk_bitfield_value(val: u32) -> PkBitfield {
    1 << val
}

#[inline]
fn pk_bitfield_contain(bitfield: PkBitfield, enum_val: u32) -> bool {
    (bitfield & pk_bitfield_value(enum_val)) > 0
}

/// Keep a percentage within a global range e.g. scale from 50-75%
fn scale_percentage(percentage: f32, start: f32, end: f32) -> f32 {
    start + (percentage * (end - start) / 100.0)
}

struct MossBackend {
    client: Client,
    installation: Installation,
}

fn get_moss_client() -> MossBackend {
    //let installation = Installation::open("/home/ninya/aeryn/img-tests/virt-manager-vm/sosroot/", None).expect("failed to open installation");
    let installation = Installation::open("/", None).expect("failed to open installation");
    let client =
        Client::new(environment::NAME, installation.clone()).expect("failed to create client");
    MossBackend {
        client,
        installation,
    }
}

/// Convert a moss package into a pk_package_id
fn moss_build_package_id_from_registry(pkg: &Package, client: &Client) -> Result<*mut c_char> {
    // Get the version of the pkg available in the repo plugin (remote)
    let available_pkg = client
        .registry
        .by_name(&pkg.meta.name, package::Flags::default())
        .filter(|p| !p.flags.installed)
        .next();

    // We have to fully resolve by id to get origin and meta.uri fully
    // populated
    let repo_resolved_pkg = if let Some(pkg) = available_pkg {
        client
            .registry
            .by_id(&pkg.id)
            .find(|pkg| !pkg.flags.installed)
    } else {
        None
    };

    let status = if pkg.flags.installed {
        match repo_resolved_pkg.and_then(|pkg| pkg.meta.origin) {
            Some(origin) => format!("installed:{}", origin),
            None => "installed".to_string(),
        }
    } else {
        repo_resolved_pkg
            .and_then(|pkg| pkg.meta.origin)
            .unwrap_or("unknown".to_string())
    };

    let c_name = CString::new(pkg.meta.name.to_string())?;
    let c_version = CString::new(pkg.meta.version_identifier.as_str())?;
    let c_arch = CString::new(pkg.meta.architecture.as_str())?;
    let c_status = CString::new(status)?;

    Ok(unsafe {
        pk_package_id_build(
            c_name.as_ptr(),
            c_version.as_ptr(),
            c_arch.as_ptr(),
            c_status.as_ptr(),
        )
    })
}

/// Gets a moss pkg from the registry from a pk_package_id
// TODO: should this be a Result? plently of error oppotunities
fn moss_get_pkg_from_package_id(package_id: *const c_char, client: &Client) -> Option<Package> {
    unsafe {
        // I assume packagekit already does this internally so may not be neccessary
        if pk_package_id_check(package_id) == 0 {
            return None;
        }

        let parts = pk_package_id_split(package_id);

        let name_ptr = *parts.add(PK_PACKAGE_ID_NAME as usize);
        let version_ptr = *parts.add(PK_PACKAGE_ID_VERSION as usize);
        let arch_ptr = *parts.add(PK_PACKAGE_ID_ARCH as usize);
        let data_ptr = *parts.add(PK_PACKAGE_ID_DATA as usize);

        if name_ptr.is_null() || version_ptr.is_null() || arch_ptr.is_null() || data_ptr.is_null() {
            return None;
        }

        let name_str = CStr::from_ptr(name_ptr).to_str().unwrap();
        let version_str = CStr::from_ptr(version_ptr).to_str().unwrap();
        let arch_str = CStr::from_ptr(arch_ptr).to_str().unwrap();
        let data_str = CStr::from_ptr(data_ptr).to_str().unwrap();

        let kind = data_str.splitn(2, ':').next().unwrap_or("");
        let flags = if kind == "installed" {
            Flags::new().with_installed()
        } else {
            Flags::new().with_available()
        };

        let provider = Provider::from_name(name_str).unwrap();
        let result = client.registry.by_provider(&provider, flags).next();

        if let Some(pkg) = result {
            if pkg.meta.architecture == arch_str && pkg.meta.version_identifier == version_str {
                Some(pkg)
            } else {
                None
            }
        } else {
            None
        }
    }
}

/// Emit a vec of moss packages to a combined pk_backend_job_packages signal
fn moss_emit_package_list(pkgs: Vec<Package>, client: &Client, job: *mut PkBackendJob) -> () {
    let pk_packages = unsafe { g_ptr_array_new_full(pkgs.len() as u32, None) };
    for pkg in pkgs {
        let id = moss_build_package_id_from_registry(&pkg, &client).pk_err(job);

        let pk_package = unsafe { pk_package_new() };
        let error: *mut *mut GError = ptr::null_mut();

        unsafe {
            if pk_package_set_id(pk_package, id, error) == 0 {
                // TODO: get the error
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_PACKAGE_ID_INVALID,
                    CString::new(format!("Failed to set package ID: {:?}", id))
                        .pk_err(job)
                        .as_ptr(),
                );
                continue;
            }
            // TODO: Take the PkInfoEnum as a param instead so this helper can be used in more places
            if pkg.flags.installed {
                pk_package_set_info(pk_package, PkInfoEnum_PK_INFO_ENUM_INSTALLED);
            } else {
                pk_package_set_info(pk_package, PkInfoEnum_PK_INFO_ENUM_AVAILABLE);
            }
            pk_package_set_summary(
                pk_package,
                CString::new(pkg.meta.summary).pk_err(job).as_ptr(),
            );
            g_ptr_array_add(pk_packages, pk_package as *mut c_void);
        }
    }
    unsafe {
        if !pk_packages.is_null() && (*pk_packages).len > 0 {
            pk_backend_job_packages(job, pk_packages);
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_description(_backend: *mut PkBackend) -> *const c_char {
    static DESCRIPTION: &str = "Moss - atomic stateful package manager\0";
    DESCRIPTION.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_author(_backend: *mut PkBackend) -> *const c_char {
    static AUTHOR: &str = "Aeryn OS Developers <copyright@aerynos.com>\0";
    AUTHOR.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_initialize(_conf: GKeyFile, _backend: *mut PkBackend) -> () {
    log_debug!("HEY HO, WHAT IS UP FROM MOSS YO...");
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_backend_destroy(_backend: *mut PkBackend) -> () {
    log_debug!("moss backend destroyed.");
}

// This function is LLM slop i have no fucking idea how to do this nicely
#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_mime_types(_backend: *mut PkBackend) -> *mut *mut c_char {
    let mime_types = vec![CString::new("application/x-stone-binary").unwrap()];

    // Convert to raw pointers
    let mut ptrs: Vec<*mut c_char> = mime_types
        .into_iter()
        .map(|cs| cs.into_raw()) // transfers ownership to C
        .collect();

    ptrs.push(ptr::null_mut()); // null terminate

    // Convert Vec to raw pointer
    let array_ptr = ptrs.as_mut_ptr();

    // Leak the Vec so the pointer remains valid
    std::mem::forget(ptrs);

    array_ptr
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_download_packages_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut package_ids: *mut *const c_char = std::ptr::null_mut();
    let mut directory: *const c_char = std::ptr::null();
    let format = CString::new("(^a&ss)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(
            gvariant_ptr,
            format.as_ptr(),
            &mut package_ids as *mut _,
            &mut directory as *mut _,
        );
    }

    let backend = get_moss_client();
    let mut client = backend.client;

    let ids = c_char_array_to_vec(package_ids);
    let ids_len = ids.len();

    let job_atomic = Arc::new(AtomicPtr::new(job));
    let job_clone = job_atomic.clone();

    for (idx, id) in ids.iter().enumerate() {
        let job_clone = job_clone.clone();
        let c_id = CString::new(id.clone()).pk_err(job);
        let id_clone = id.clone();
        match client.fetch(
            &vec![id.as_str()],
            Path::new("."),
            false,
            Some(Arc::new(move |download_callback| {
                let job_ptr = job_clone.load(Ordering::Relaxed);
                match download_callback {
                    DownloadCallback::Current(_pkg, pct) => {
                        let c_id = CString::new(id_clone.clone()).pk_err(job_ptr);
                        let pk_percentage = (pct * 100.0).floor() as u32;
                        let global_pct = ((idx as f32) + pct) / (ids_len as f32);
                        let global_percentage = (global_pct * 100.0).floor() as u32;
                        unsafe {
                            pk_backend_job_set_percentage(job_ptr, global_percentage);
                        }

                        unsafe {
                            pk_backend_job_set_item_progress(
                                job_ptr,
                                c_id.as_ptr(),
                                PkInfoEnum_PK_INFO_ENUM_DOWNLOADING,
                                pk_percentage,
                            );
                        }
                    }
                    DownloadCallback::Overall(_) => {
                        log_debug!("DownloadCallback::Overall not yet implemented")
                    }
                }
            })),
        ) {
            Ok((paths, _)) => {
                let mut c_target = OurCStringArray::from_vec(
                    paths
                        .iter()
                        .map(|p| p.to_string_lossy().to_string().to_owned()),
                );

                unsafe {
                    pk_backend_job_files(job, c_id.as_ptr(), c_target.as_ptr());
                }
            }
            Err(e) => {
                let c_err = CString::new(e.to_string()).pk_err(job);
                unsafe {
                    pk_backend_job_error_code(
                        job,
                        PkErrorEnum_PK_ERROR_ENUM_PACKAGE_DOWNLOAD_FAILED,
                        c_err.as_ptr(),
                    );
                }
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_download_packages(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _package_ids: *const *const c_char,
    _directory: *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_download_packages_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_details_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut package_ids: *mut *const c_char = std::ptr::null_mut();
    let format = CString::new("(^a&s)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(gvariant_ptr, format.as_ptr(), &mut package_ids as *mut _);
    }

    let backend = get_moss_client();
    let client = &backend.client;

    let ids = c_char_array_to_vec(package_ids);

    for id in ids {
        let c_id = CString::new(id.clone()).pk_err(job);
        if let Some(pkg) = moss_get_pkg_from_package_id(c_id.as_ptr(), client) {
            let c_sum = CString::new(pkg.meta.summary).pk_err(job);
            let c_lic = CString::new(pkg.meta.licenses.first().unwrap().to_string()).pk_err(job);
            let c_desc = CString::new(pkg.meta.description).pk_err(job);
            let c_url = CString::new(pkg.meta.homepage).pk_err(job);
            unsafe {
                pk_backend_job_details(
                    job,
                    c_id.as_ptr(),
                    c_sum.as_ptr(),
                    c_lic.as_ptr(),
                    PkGroupEnum_PK_GROUP_ENUM_UNKNOWN,
                    c_desc.as_ptr(),
                    c_url.as_ptr(),
                    u64::MAX, // FIXME: No way to get installed size of a package?
                    pkg.meta.download_size.unwrap_or_else(|| u64::MAX),
                );
            }
        } else {
            unsafe {
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND,
                    CString::new(format!("Failed to find package {:?}", id))
                        .pk_err(job)
                        .as_ptr(),
                );
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_details(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _package_ids: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(job, Some(backend_get_details_thread), ptr::null_mut(), None);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_details_local_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut full_paths: *mut *const c_char = std::ptr::null_mut();
    let format = CString::new("(^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(gvariant_ptr, format.as_ptr(), &mut full_paths as *mut _);
    }

    let paths = c_char_array_to_vec(full_paths);

    for path in paths {
        let mut file = File::open(&path).unwrap();
        let mut reader = stone::read(&mut file).unwrap();

        let payloads = reader.payloads().unwrap();

        let mut pkg_name: Option<String> = None;
        let mut pkg_arch: Option<String> = None;
        let mut pkg_ver: Option<String> = None;
        let pkg_status = "local";
        let mut pkg_summary: Option<String> = None;
        let mut pkg_license: Option<String> = None;
        let mut pkg_desc: Option<String> = None;
        let mut pkg_homepage: Option<String> = None;
        let mut pkg_dlsize: Option<u64> = None;

        for payload in payloads.flatten() {
            match payload {
                StoneDecodedPayload::Meta(meta) => {
                    for record in meta.body {
                        match &record.tag {
                            // HOLY MOTHER OF NESTING, BETTER WAY!?
                            StonePayloadMetaTag::Name => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_name = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            StonePayloadMetaTag::Version => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_arch = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            StonePayloadMetaTag::Architecture => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_ver = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            StonePayloadMetaTag::Summary => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_summary = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            StonePayloadMetaTag::License => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_license = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            StonePayloadMetaTag::Description => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_desc = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            StonePayloadMetaTag::Homepage => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_homepage = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            StonePayloadMetaTag::PackageSize => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::Uint64(u) => {
                                        pkg_dlsize = Some(u);
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        let c_name = CString::new(pkg_name.unwrap().as_str()).pk_err(job);
        let c_ver = CString::new(pkg_ver.unwrap().as_str()).pk_err(job);
        let c_arch = CString::new(pkg_arch.unwrap().as_str()).pk_err(job);
        let c_sum = CString::new(pkg_summary.unwrap().as_str()).pk_err(job);
        let c_desc = CString::new(pkg_desc.unwrap().as_str()).pk_err(job);
        let c_home = CString::new(pkg_homepage.unwrap().as_str()).pk_err(job);
        let c_lic = CString::new(pkg_license.unwrap().as_str()).pk_err(job);

        unsafe {
            let id = pk_package_id_build(
                c_name.as_ptr(),
                c_ver.as_ptr(),
                c_arch.as_ptr(),
                CString::new(pkg_status).unwrap().as_ptr(),
            );
            pk_backend_job_details(
                job,
                id,
                c_sum.as_ptr(),
                c_lic.as_ptr(),
                PkGroupEnum_PK_GROUP_ENUM_UNKNOWN,
                c_desc.as_ptr(),
                c_home.as_ptr(),
                u64::MAX, // FIXME: No way to get installed size of a package? NOTE: will print unknown once this lands https://github.com/PackageKit/PackageKit/pull/851,
                pkg_dlsize.unwrap_or(u64::MAX),
            )
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_details_local(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _full_paths: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_get_details_local_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_packages_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut filters: PkBitfield = 0;
    let format = CString::new("(t)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(gvariant_ptr, format.as_ptr(), &mut filters as *mut _);
    }

    let backend = get_moss_client();
    let client = &backend.client;

    let flags = if pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_INSTALLED) {
        package::Flags::new().with_installed()
    } else if pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NOT_INSTALLED) {
        package::Flags::new().with_available()
    } else {
        package::Flags::default()
    };

    // TODO: ~newest filter how that does work? i think we get the latest version of a package from
    //       all repos
    let is_newest = pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NEWEST)
        || pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NOT_NEWEST);

    let mut pkgs_to_emit = Vec::new();

    let mut seen = HashSet::new();
    for pkg in client.registry.list(flags) {
        // We have to filter out the remote versions of packages which are already installed :(
        if is_newest || seen.insert(pkg.meta.name.to_string().clone()) {
            pkgs_to_emit.push(pkg);
        }
    }

    moss_emit_package_list(pkgs_to_emit, client, job);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_packages(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _filters: PkBitfield,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_get_packages_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_resolve_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut search: *mut *const c_char = std::ptr::null_mut();
    let mut filters: PkBitfield = 0;
    let format = CString::new("(t^a&s)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(
            gvariant_ptr,
            format.as_ptr(),
            &mut filters as *mut _,
            &mut search as *mut _,
        );
    }

    let backend = get_moss_client();
    let client = &backend.client;

    let search_terms = c_char_array_to_vec(search);

    let flags = if pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_INSTALLED) {
        package::Flags::new().with_installed()
    } else if pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NOT_INSTALLED) {
        package::Flags::new().with_available()
    } else {
        package::Flags::default()
    };

    // TODO: ~newest filter how that does work? i think we get the latest version of a package
    //       from all repos, even if they're not the most up to date.
    let is_newest = pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NEWEST)
        || pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NOT_NEWEST);

    let mut pkgs_to_emit = Vec::new();

    let mut seen_installed = HashSet::new();
    for keyword in &search_terms {
        let matches: Vec<_> = client
            .registry
            .by_keyword(keyword, flags)
            .filter(|pkg| pkg.meta.name.to_string() == *keyword)
            .filter(|pkg| {
                // newest filter operates on installed and available
                // lists seperately so don't filter out duplicates
                if is_newest {
                    return true;
                }
                if pkg.flags.installed {
                    seen_installed.insert(pkg.id.to_string());
                    true
                } else if pkg.flags.available {
                    !seen_installed.contains(&pkg.id.to_string())
                } else {
                    true
                }
            })
            .collect();

        for pkg in matches {
            pkgs_to_emit.push(pkg);
        }
    }

    moss_emit_package_list(pkgs_to_emit, client, job);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_resolve(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _filters: PkBitfield,
    _search: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(job, Some(backend_resolve_thread), ptr::null_mut(), None);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_files_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut package_ids: *mut *const c_char = std::ptr::null_mut();
    let format = CString::new("(^a&s)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(gvariant_ptr, format.as_ptr(), &mut package_ids as *mut _);
    }

    let backend = get_moss_client();
    let client = &backend.client;

    let ids = c_char_array_to_vec(package_ids);
    for id in ids {
        let c_id = CString::new(id.clone()).pk_err(job);
        if let Some(pkg) = moss_get_pkg_from_package_id(c_id.as_ptr(), client) {
            let vfs = client.vfs(&[pkg.id]).pk_err(job);
            let files = vfs
                .iter()
                .filter_map(|file| {
                    if matches!(file.kind(), vfs::tree::Kind::Directory) {
                        return None;
                    }
                    let path = file.path();
                    Some(path)
                })
                .collect::<Vec<_>>();

            let mut files_ptr = OurCStringArray::from_vec(files);

            unsafe {
                pk_backend_job_files(job, c_id.as_ptr(), files_ptr.as_ptr());
            }
        } else {
            unsafe {
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND,
                    CString::new(format!("Failed to find package {:?}", id))
                        .pk_err(job)
                        .as_ptr(),
                );
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_files(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _package_ids: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(job, Some(backend_get_files_thread), ptr::null_mut(), None);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_files_local_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut full_paths: *mut *const c_char = std::ptr::null_mut();
    let format = CString::new("(^a&s)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(gvariant_ptr, format.as_ptr(), &mut full_paths as *mut _);
    }

    let paths = c_char_array_to_vec(full_paths);

    for path in paths {
        let mut file = File::open(&path).unwrap();
        let mut reader = stone::read(&mut file).unwrap();

        let payloads = reader.payloads().unwrap();

        let mut pkg_name: Option<String> = None;
        let mut pkg_arch: Option<String> = None;
        let mut pkg_ver: Option<String> = None;
        let pkg_status = "local";

        let mut layouts = vec![];

        for payload in payloads.flatten() {
            match payload {
                StoneDecodedPayload::Layout(l) => layouts = l.body,
                StoneDecodedPayload::Meta(meta) => {
                    for record in meta.body {
                        match &record.tag {
                            // HOLY MOTHER OF NESTING, BETTER WAY!?
                            StonePayloadMetaTag::Name => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_name = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            StonePayloadMetaTag::Version => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_arch = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            StonePayloadMetaTag::Architecture => {
                                let kind = record.primitive;
                                match kind {
                                    StonePayloadMetaPrimitive::String(s) => {
                                        pkg_ver = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        let c_name = CString::new(pkg_name.unwrap().as_str()).unwrap();
        let c_ver = CString::new(pkg_ver.unwrap().as_str()).unwrap();
        let c_arch = CString::new(pkg_arch.unwrap().as_str()).unwrap();

        let files = layouts
            .iter()
            .filter_map(|file| match &file.file {
                StonePayloadLayoutFile::Regular(_, target)
                | StonePayloadLayoutFile::Directory(target)
                | StonePayloadLayoutFile::Symlink(_, target) => {
                    Some(format!("/usr/{}", target.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        let mut files_ptr = OurCStringArray::from_vec(files);

        unsafe {
            let id = pk_package_id_build(
                c_name.as_ptr(),
                c_ver.as_ptr(),
                c_arch.as_ptr(),
                CString::new(pkg_status).unwrap().as_ptr(),
            );
            pk_backend_job_files(job, id, files_ptr.as_ptr());
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_files_local(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _full_paths: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_get_files_local_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_search_files_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut search: *mut *const c_char = std::ptr::null_mut();
    let mut filters: PkBitfield = 0;
    let format = CString::new("(t^a&s)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(
            gvariant_ptr,
            format.as_ptr(),
            &mut filters as *mut _,
            &mut search as *mut _,
        );
    }

    let backend = get_moss_client();
    let client = &backend.client;

    let search_terms = c_char_array_to_vec(search);

    let layouts = client.list_layouts().pk_err(job);

    let mut pkgs_to_emit = Vec::new();

    layouts
        .into_iter()
        .for_each(|(id, layout)| match layout.file {
            StonePayloadLayoutFile::Regular(_, file)
            | StonePayloadLayoutFile::Symlink(_, file)
            | StonePayloadLayoutFile::Directory(file) => {
                for keyword in &search_terms {
                    if file.contains(keyword) {
                        if let Some(pkg) = client.registry.by_id(&id).next() {
                            pkgs_to_emit.push(pkg);
                        }
                    }
                }
            }
            _ => {}
        });

    moss_emit_package_list(pkgs_to_emit, client, job);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_search_files(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _filters: PkBitfield,
    _values: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_search_files_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_search_details_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut search: *mut *const c_char = std::ptr::null_mut();
    let mut filters: PkBitfield = 0;
    let format = CString::new("(t^a&s)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(
            gvariant_ptr,
            format.as_ptr(),
            &mut filters as *mut _,
            &mut search as *mut _,
        );
    }

    let backend = get_moss_client();
    let client = &backend.client;

    let search_terms = c_char_array_to_vec(search);

    let flags = if pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_INSTALLED) {
        package::Flags::new().with_installed()
    } else if pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NOT_INSTALLED) {
        package::Flags::new().with_available()
    } else {
        package::Flags::default()
    };

    let mut pkgs_to_emit = Vec::new();

    let mut seen = HashSet::new();
    for keyword in &search_terms {
        let matches: Vec<_> = client
            .registry
            .by_keyword(keyword, flags)
            .filter(|pkg| {
                pkg.meta.summary.contains(keyword) || pkg.meta.description.contains(keyword)
            })
            .collect();

        // We have to filter out the remote versions of packages which are already installed :(
        // however, for the newest filter we operate on available and installed lists separately
        // TODO: ~newest filter how that does work?
        let is_newest = pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NEWEST)
            || pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NOT_NEWEST);
        for pkg in matches {
            if is_newest || seen.insert(pkg.meta.name.to_string().clone()) {
                pkgs_to_emit.push(pkg);
            }
        }
    }

    moss_emit_package_list(pkgs_to_emit, client, job);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_search_details(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _filters: PkBitfield,
    _values: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_search_details_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_search_names_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut search: *mut *const c_char = std::ptr::null_mut();
    let mut filters: PkBitfield = 0;
    let format = CString::new("(t^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(
            gvariant_ptr,
            format.as_ptr(),
            &mut filters as *mut _,
            &mut search as *mut _,
        );
    }

    let backend = get_moss_client();
    let client = &backend.client;

    let search_terms = c_char_array_to_vec(search);

    let flags = if pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_INSTALLED) {
        package::Flags::new().with_installed()
    } else if pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NOT_INSTALLED) {
        package::Flags::new().with_available()
    } else {
        package::Flags::default()
    };

    let mut pkgs_to_emit = Vec::new();

    let mut seen = HashSet::new();
    for keyword in &search_terms {
        let matches: Vec<_> = client
            .registry
            .by_keyword(keyword, flags)
            .filter(|pkg| pkg.meta.name.to_string().contains(keyword))
            .collect();

        // We have to filter out the remote versions of packages which
        // are already installed :(
        // however, for the newest filter we operate on available and installed lists separately
        // TODO: ~newest filter how that does work? i think we get the latest version of a package from
        //       all repos
        let is_newest = pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NEWEST)
            || pk_bitfield_contain(filters, PkFilterEnum_PK_FILTER_ENUM_NOT_NEWEST);
        for pkg in matches {
            if is_newest || seen.insert(pkg.meta.name.to_string().clone()) {
                pkgs_to_emit.push(pkg);
            }
        }
    }

    moss_emit_package_list(pkgs_to_emit, client, job);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_search_names(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _filters: PkBitfield,
    _values: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_search_names_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_remove_packages_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut package_ids: *mut *const c_char = std::ptr::null_mut();
    let mut transaction_flags: PkBitfield = 0;
    let mut allow_deps: i32 = 0;
    let mut autoremove: i32 = 0;
    let format = CString::new("(t^a&sbb)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(
            gvariant_ptr,
            format.as_ptr(),
            &mut transaction_flags as *mut _,
            &mut package_ids as *mut _,
            &mut allow_deps as *mut _,
            &mut autoremove as *mut _,
        );
    }

    let simulate = pk_bitfield_contain(
        transaction_flags,
        PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_SIMULATE,
    );

    let backend = get_moss_client();
    let mut client = backend.client;

    let mut resolved = Vec::new();
    let ids = c_char_array_to_vec(package_ids);

    for id in ids {
        let c_id = CString::new(id.clone()).pk_err(job);
        if let Some(pkg) = moss_get_pkg_from_package_id(c_id.as_ptr(), &client) {
            resolved.push(pkg);
        } else {
            unsafe {
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND,
                    CString::new(format!("Failed to find package {:?}", id))
                        .pk_err(job)
                        .as_ptr(),
                );
            }
        }
    }

    let removed_str = resolved
        .iter()
        .map(|p| p.meta.name.as_str())
        .collect::<Vec<_>>();

    let job_atomic = Arc::new(AtomicPtr::new(job));
    let job_clone = job_atomic.clone();

    let (pkgs, _) = client
        .remove(
            &removed_str,
            true,
            simulate,
            Some(Arc::new(move |percentage, stage| {
                let job_ptr = job_clone.load(Ordering::Relaxed);
                let adjusted_percentage = match stage {
                    ProgressStage::Resolve => {
                        unsafe {
                            pk_backend_job_set_status(
                                job_ptr,
                                PkStatusEnum_PK_STATUS_ENUM_DEP_RESOLVE,
                            );
                        }
                        scale_percentage(percentage, 0.0, 20.0)
                    }
                    // We'll never recieve download callbacks in a remove context
                    ProgressStage::Downloading => unreachable!(),
                    ProgressStage::Blit => {
                        unsafe {
                            pk_backend_job_set_status(job_ptr, PkStatusEnum_PK_STATUS_ENUM_REMOVE);
                        }
                        scale_percentage(percentage, 20.0, 50.0)
                    }
                    ProgressStage::Transaction => scale_percentage(percentage, 50.0, 70.0),
                    ProgressStage::System => scale_percentage(percentage, 70.0, 90.0),
                    ProgressStage::Boot => scale_percentage(percentage, 90.0, 100.0),
                };
                unsafe {
                    pk_backend_job_set_percentage(job_ptr, adjusted_percentage as u32);
                }
            })),
        )
        .pk_err(job);

    if simulate {
        for pkg in pkgs {
            unsafe {
                let id = moss_build_package_id_from_registry(&pkg, &client).pk_err(job);
                let c_summary = CString::new(pkg.meta.summary.clone()).unwrap();
                pk_backend_job_package(
                    job,
                    PkInfoEnum_PK_INFO_ENUM_REMOVING,
                    id,
                    c_summary.as_ptr(),
                );
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_remove_packages(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _transaction_flags: PkBitfield,
    _package_ids: *const *const c_char,
    _allow_deps: i32,
    _autoremove: i32,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_remove_packages_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_install_packages_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut package_ids: *mut *const c_char = std::ptr::null_mut();
    let mut transaction_flags: PkBitfield = 0;
    let format = CString::new("(t^a&s)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(
            gvariant_ptr,
            format.as_ptr(),
            &mut transaction_flags as *mut _,
            &mut package_ids as *mut _,
        );
    }

    let backend = get_moss_client();
    let mut client = backend.client;

    let simulate = pk_bitfield_contain(
        transaction_flags,
        PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_SIMULATE,
    );

    let mut resolved = Vec::new();

    let ids = c_char_array_to_vec(package_ids);

    for id in ids {
        let c_id = CString::new(id.clone()).pk_err(job);
        if let Some(pkg) = moss_get_pkg_from_package_id(c_id.as_ptr(), &client) {
            resolved.push(pkg);
        } else {
            unsafe {
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND,
                    CString::new(format!("Failed to find package {:?}", id))
                        .pk_err(job)
                        .as_ptr(),
                );
            }
        }
    }

    let job_atomic = Arc::new(AtomicPtr::new(job));
    let job_clone = job_atomic.clone();

    let packages = resolved
        .iter()
        .map(|p| p.meta.name.as_str())
        .collect::<Vec<_>>();

    let (pkgs, _) = client
        .install(
            &packages,
            true,
            simulate,
            Some(Arc::new(move |percentage, stage| {
                let job_ptr = job_clone.load(Ordering::Relaxed);
                let adjusted_percentage = match stage {
                    ProgressStage::Resolve => {
                        unsafe {
                            pk_backend_job_set_status(
                                job_ptr,
                                PkStatusEnum_PK_STATUS_ENUM_DEP_RESOLVE,
                            );
                        }
                        scale_percentage(percentage, 0.0, 10.0)
                    }
                    ProgressStage::Downloading => {
                        unsafe {
                            pk_backend_job_set_status(
                                job_ptr,
                                PkStatusEnum_PK_STATUS_ENUM_DOWNLOAD,
                            );
                        }
                        scale_percentage(percentage, 10.0, 40.0)
                    }
                    ProgressStage::Blit => {
                        unsafe {
                            pk_backend_job_set_status(job_ptr, PkStatusEnum_PK_STATUS_ENUM_INSTALL);
                        }
                        scale_percentage(percentage, 40.0, 70.0)
                    }
                    ProgressStage::Transaction => scale_percentage(percentage, 70.0, 80.0),
                    ProgressStage::System => scale_percentage(percentage, 80.0, 90.0),
                    ProgressStage::Boot => scale_percentage(percentage, 90.0, 100.0),
                };
                unsafe {
                    pk_backend_job_set_percentage(job_ptr, adjusted_percentage as u32);
                }
            })),
        )
        .pk_err(job);

    if simulate {
        for pkg in pkgs {
            unsafe {
                let id = moss_build_package_id_from_registry(&pkg, &client).pk_err(job);
                let c_summary = CString::new(pkg.meta.summary.clone()).unwrap();
                pk_backend_job_package(
                    job,
                    PkInfoEnum_PK_INFO_ENUM_INSTALLING,
                    id,
                    c_summary.as_ptr(),
                );
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_install_packages(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _transaction_flags: PkBitfield,
    _package_ids: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_install_packages_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_update_packages_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut package_ids: *mut *const c_char = std::ptr::null_mut();
    let mut transaction_flags: PkBitfield = 0;
    let format = CString::new("(t^a&s)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(
            gvariant_ptr,
            format.as_ptr(),
            &mut transaction_flags as *mut _,
            &mut package_ids as *mut _,
        );
    }

    let backend = get_moss_client();
    let mut client = backend.client;

    let only_download = pk_bitfield_contain(
        transaction_flags,
        PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_ONLY_DOWNLOAD,
    );
    if only_download {
        unsafe {
            pk_backend_job_set_status(job, PkStatusEnum_PK_STATUS_ENUM_DOWNLOAD);
        }
    }

    let simulate = pk_bitfield_contain(
        transaction_flags,
        PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_SIMULATE,
    );

    unsafe {
        pk_backend_job_set_status(job, PkStatusEnum_PK_STATUS_ENUM_UPDATE);
    }

    let job_atomic = Arc::new(AtomicPtr::new(job));
    let job_clone = job_atomic.clone();

    let (pkgs, _) = client
        .sync(
            None,
            true,
            simulate,
            only_download,
            Some(Arc::new(move |percentage, stage| {
                let job_ptr = job_clone.load(Ordering::Relaxed);
                let adjusted_percentage = match stage {
                    ProgressStage::Resolve => scale_percentage(percentage, 0.0, 20.0),
                    ProgressStage::Downloading => scale_percentage(percentage, 20.0, 40.0),
                    ProgressStage::Blit => scale_percentage(percentage, 40.0, 70.0),
                    ProgressStage::Transaction => scale_percentage(percentage, 70.0, 80.0),
                    ProgressStage::System => scale_percentage(percentage, 80.0, 90.0),
                    ProgressStage::Boot => scale_percentage(percentage, 90.0, 100.0),
                };
                unsafe {
                    pk_backend_job_set_percentage(job_ptr, adjusted_percentage as u32);
                }
            })),
        )
        .pk_err(job);

    if simulate {
        for pkg in pkgs {
            unsafe {
                let id = moss_build_package_id_from_registry(&pkg, &client).pk_err(job);
                let c_summary = CString::new(pkg.meta.summary.clone()).unwrap();
                pk_backend_job_package(
                    job,
                    PkInfoEnum_PK_INFO_ENUM_UPDATING,
                    id,
                    c_summary.as_ptr(),
                );
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_update_packages(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _transaction_flags: PkBitfield,
    _package_ids: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_update_packages_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_update_detail_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut package_ids: *mut *const c_char = std::ptr::null_mut();
    let format = CString::new("(^a&s)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(gvariant_ptr, format.as_ptr(), &mut package_ids as *mut _);
    }

    let backend = get_moss_client();
    let client = &backend.client;

    let ids = c_char_array_to_vec(package_ids);

    for id in ids {
        let c_id = CString::new(id.clone()).pk_err(job);
        if let Some(_) = moss_get_pkg_from_package_id(c_id.as_ptr(), client) {
            unsafe {
                pk_backend_job_update_detail(
                    job,
                    c_id.as_ptr(),
                    ptr::null_mut(),                                // updates
                    ptr::null_mut(),                                // obsoletes
                    ptr::null_mut(),                                // vendor urls
                    ptr::null_mut(),                                // bugzilla urls
                    ptr::null_mut(),                                // cve urls
                    PkRestartEnum_PK_RESTART_ENUM_NONE,             // package warrants restart?
                    ptr::null_mut(),                                // update text
                    ptr::null_mut(),                                // changelog
                    PkUpdateStateEnum_PK_UPDATE_STATE_ENUM_UNKNOWN, // update state
                    ptr::null_mut(),                                // issued (date)
                    ptr::null_mut(),                                // updated (date)
                );
            }
        } else {
            unsafe {
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND,
                    CString::new(format!("Failed to find package {:?}", id))
                        .pk_err(job)
                        .as_ptr(),
                );
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_update_detail(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _package_ids: *const *const c_char,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_get_update_detail_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_updates_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut filters: PkBitfield = 0;
    let format = CString::new("(t)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(gvariant_ptr, format.as_ptr(), &mut filters as *mut _);
    }

    let backend = get_moss_client();
    let client = &backend.client;

    let pkgs_installed = client
        .registry
        .list(Flags::new().with_installed())
        .collect::<Vec<_>>();
    let pkgs_available = client
        .registry
        .list(Flags::new().with_available())
        .collect::<Vec<_>>();

    // NOTE: we doing moss list sync --upgrade-only here for now
    //       for downgrades with higher priority we need to think about consequences in a front-end
    //       application such as gnome-software e.g. outdated pkgs in local or community repo
    //       which may not be ABI compatible
    let mut set = pkgs_installed
        .into_iter()
        .filter_map(|p| {
            pkgs_available
                .iter()
                .find(|u| u.meta.name == p.meta.name)
                .filter(|u| u.meta.source_release > p.meta.source_release)
        })
        .collect::<Vec<_>>();
    set.sort_by_key(|s| s.meta.name.clone());
    set.dedup_by_key(|s| s.meta.name.clone());

    for pkg in set {
        unsafe {
            let id = moss_build_package_id_from_registry(&pkg, &client).pk_err(job);
            let c_summary = CString::new(pkg.meta.summary.clone()).pk_err(job);

            // TODO: no way to determine pkgs which are security fixes, other enum types are also available
            pk_backend_job_package(job, PkInfoEnum_PK_INFO_ENUM_NORMAL, id, c_summary.as_ptr());
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_updates(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _filters: PkBitfield,
) -> () {
    unsafe {
        pk_backend_job_thread_create(job, Some(backend_get_updates_thread), ptr::null_mut(), None);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_refresh_cache_thread(
    job: *mut PkBackendJob,
    params: *mut _GVariant,
    _user_data: *mut c_void,
) -> () {
    let mut force: i32 = 0;
    let format = CString::new("(b)").pk_err(job);
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe {
        g_variant_get(gvariant_ptr, format.as_ptr(), &mut force as *mut _);
    }

    let backend = get_moss_client();
    let config = config::Manager::system(&backend.installation.root, "moss");

    unsafe {
        pk_backend_job_set_status(job, PkStatusEnum_PK_STATUS_ENUM_REFRESH_CACHE);
    }

    match repository::Manager::system(config, backend.installation.clone()) {
        Ok(manager) => {
            let repo_len = manager.list().len();
            for (idx, repo) in manager.list().enumerate() {
                match runtime::block_on(async { manager.refresh(repo.0).await }) {
                    Ok(_) => {
                        let percentage = if idx > repo_len {
                            100
                        } else {
                            (100 * idx) / repo_len
                        } as u32;
                        //log_debug!("refresh pct {}", percentage);
                        unsafe {
                            pk_backend_job_set_percentage(job, percentage);
                        }
                    }
                    Err(e) => {
                        let c_err = e.to_string();
                        unsafe {
                            pk_backend_job_error_code(
                                job,
                                PkErrorEnum_PK_ERROR_ENUM_REPO_CONFIGURATION_ERROR,
                                CString::new(c_err).pk_err(job).as_ptr(),
                            );
                        }
                    }
                }
            }
        }
        Err(e) => {
            let c_err = e.to_string();
            unsafe {
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION,
                    CString::new(c_err).pk_err(job).as_ptr(),
                );
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_refresh_cache(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _force: i32,
) -> () {
    unsafe {
        pk_backend_job_thread_create(
            job,
            Some(backend_refresh_cache_thread),
            ptr::null_mut(),
            None,
        );
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_repo_enable(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    rid: *const c_char,
    enabled: i32,
) -> () {
    let backend = get_moss_client();
    let config = config::Manager::system(&backend.installation.root, "moss");

    unsafe {
        let c_str = CStr::from_ptr(rid);
        let rid_str = match c_str.to_str() {
            Ok(s) => s,
            Err(_) => return,
        };

        let pk_id = repository::Id::new(&rid_str);
        let enabled: bool = enabled != 0;

        match repository::Manager::system(config, backend.installation.clone()) {
            Ok(mut manager) => {
                // NOTE: borrowing issues with mutable vs immutable manager
                let repo_ids: Vec<_> = manager
                    .list()
                    .map(|(id, repo)| (id.clone(), repo.clone()))
                    .collect();
                let mut found_repo = false;
                for (id, _repo) in repo_ids {
                    if id.clone() == pk_id {
                        found_repo = true;
                        if enabled {
                            match runtime::block_on(manager.enable(&id)) {
                                Ok(_) => {}
                                Err(e) => {
                                    let c_err = e.to_string();
                                    pk_backend_job_error_code(
                                        job,
                                        PkErrorEnum_PK_ERROR_ENUM_REPO_CONFIGURATION_ERROR,
                                        CString::new(c_err).unwrap().as_ptr(),
                                    );
                                }
                            }
                        } else {
                            match runtime::block_on(manager.disable(&id)) {
                                Ok(_) => {}
                                Err(e) => {
                                    let c_err = e.to_string();
                                    pk_backend_job_error_code(
                                        job,
                                        PkErrorEnum_PK_ERROR_ENUM_REPO_CONFIGURATION_ERROR,
                                        CString::new(c_err).unwrap().as_ptr(),
                                    );
                                }
                            }
                        }
                    }
                }
                if found_repo == false {
                    let c_err = CString::new(format!("Failed to find repo: {}", pk_id)).unwrap();
                    pk_backend_job_error_code(
                        job,
                        PkErrorEnum_PK_ERROR_ENUM_REPO_NOT_FOUND,
                        c_err.as_ptr(),
                    );
                }
            }
            Err(e) => {
                let c_err = e.to_string();
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION,
                    CString::new(c_err).unwrap().as_ptr(),
                );
            }
        }
    }
    unsafe {
        pk_backend_job_finished(job);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_repo_set_data(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    repo_id: *const c_char,
    parameter: *const c_char,
    value: *const c_char,
) -> () {
    let backend = get_moss_client();
    let config = config::Manager::system(&backend.installation.root, "moss");

    match repository::Manager::system(config, backend.installation.clone()) {
        Ok(mut manager) => {
            unsafe {
                let c_rid = CStr::from_ptr(repo_id);
                let rid_str = match c_rid.to_str() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let c_param = CStr::from_ptr(parameter);
                let param_str = match c_param.to_str() {
                    Ok(s) => s,
                    Err(_) => return,
                };

                let c_value = CStr::from_ptr(value);
                let value_str = match c_value.to_str() {
                    Ok(s) => s,
                    Err(_) => return,
                };

                let pk_id = repository::Id::new(&rid_str);

                match param_str {
                    "add" => {
                        let uri = Url::parse(value_str).unwrap();
                        manager
                            .add_repository(
                                pk_id.clone(),
                                Repository {
                                    description: "...".to_string(),
                                    uri,
                                    priority: Priority::new(0),
                                    active: true,
                                },
                            )
                            .unwrap();
                        // TODO: should we actually refresh the repo here or rely on refresh-cache?
                        match runtime::block_on(manager.refresh(&pk_id)) {
                            Ok(_) => {}
                            Err(e) => {
                                let c_err = CString::new(e.to_string()).unwrap();
                                pk_backend_job_error_code(
                                    job,
                                    PkErrorEnum_PK_ERROR_ENUM_REPO_NOT_FOUND,
                                    c_err.as_ptr(),
                                );
                            }
                        }
                    }
                    "remove" => match manager.remove(pk_id.clone()).unwrap() {
                        repository::manager::Removal::NotFound => {
                            let c_err =
                                CString::new(format!("Repository id: {} was not found", pk_id))
                                    .unwrap();
                            pk_backend_job_error_code(
                                job,
                                PkErrorEnum_PK_ERROR_ENUM_REPO_NOT_FOUND,
                                c_err.as_ptr(),
                            );
                        }
                        repository::manager::Removal::ConfigDeleted(false) => {}
                        repository::manager::Removal::ConfigDeleted(true) => {}
                    },
                    // TODO: modify priority and url of existing repos?
                    _ => {
                        let c_err =
                            CString::new("Valid parameters for set_repo_data are: add and, remove")
                                .unwrap();
                        pk_backend_job_error_code(
                            job,
                            PkErrorEnum_PK_ERROR_ENUM_NOT_SUPPORTED,
                            c_err.as_ptr(),
                        );
                    }
                }
            }
        }
        Err(e) => {
            let c_err = e.to_string();
            unsafe {
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION,
                    CString::new(c_err).unwrap().as_ptr(),
                );
            }
        }
    }
    unsafe {
        pk_backend_job_finished(job);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_repo_list(
    _backend: *mut PkBackend,
    job: *mut PkBackendJob,
    _filters: PkBitfield,
) -> () {
    let backend = get_moss_client();
    let config = config::Manager::system(&backend.installation.root, "moss");

    match repository::Manager::system(config, backend.installation.clone()) {
        Ok(manager) => {
            let configured_repos = manager.list();
            if configured_repos.len() == 0 {
                // TODO, set pk_backend_job_error_code?
                return;
            }
            for (id, repo) in
                configured_repos.sorted_by(|(_, a), (_, b)| a.priority.cmp(&b.priority).reverse())
            {
                let c_id = CString::new(id.to_string()).unwrap();
                let c_desc = CString::new(repo.description.clone()).unwrap();
                let c_active = if repo.active { 1 } else { 0 };
                unsafe {
                    pk_backend_job_repo_detail(job, c_id.as_ptr(), c_desc.as_ptr(), c_active);
                }
            }
        }
        Err(e) => {
            let c_err = e.to_string();
            unsafe {
                pk_backend_job_error_code(
                    job,
                    PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION,
                    CString::new(c_err).unwrap().as_ptr(),
                );
            }
        }
    }
    unsafe {
        pk_backend_job_finished(job);
    }
}

unsafe fn c_strings_to_vec_null_terminated(c_strings: *const *const c_char) -> Vec<String> {
    if c_strings.is_null() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut i = 0;

    loop {
        unsafe {
            let c_str_ptr = *c_strings.add(i);
            if c_str_ptr.is_null() {
                break; // Found null terminator
            }

            let c_str = CStr::from_ptr(c_str_ptr);
            if let Ok(rust_str) = c_str.to_str() {
                result.push(rust_str.to_string());
            }
            i += 1;
        }
    }
    result
}

// debugging
unsafe fn print_c_char_array(ptr: *mut *const c_char) {
    if ptr.is_null() {
        println!("ptr is null");
        return;
    }

    let mut i = 0;
    unsafe {
        loop {
            let c_str_ptr = *ptr.add(i);
            if c_str_ptr.is_null() {
                break;
            }

            let cstr = CStr::from_ptr(c_str_ptr);
            match cstr.to_str() {
                Ok(s) => println!("Item {}: {}", i, s),
                Err(e) => println!("Item {}: invalid UTF-8 ({})", i, e),
            }

            i += 1;
        }
    }
}

// LLM slop, lifetimes are an issue here so be careful and validate output
// TODO: replace with ffi-convert crate
pub struct OurCStringArray {
    cstrings: Vec<CString>,
    ptrs: Vec<*mut c_char>,
}

impl OurCStringArray {
    /// Converts a Vec<String> to a CStringArray with null-terminated pointer array.
    pub fn from_vec<I, S>(strings: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        let cstrings: Vec<CString> = strings
            .into_iter()
            .map(|s| CString::new(s.as_ref()).unwrap())
            .collect();

        // Create vector of raw pointers
        let mut ptrs: Vec<*mut c_char> = cstrings
            .iter()
            .map(|cs| cs.as_ptr() as *mut c_char)
            .collect();

        ptrs.push(ptr::null_mut());

        OurCStringArray { cstrings, ptrs }
    }

    /// Returns the pointer to the first pointer (char **)
    pub fn as_ptr(&mut self) -> *mut *mut c_char {
        self.ptrs.as_mut_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::packagekit::{pk_get_distro_id, pk_package_id_build, pk_package_id_check};

    #[test]
    fn pk_distro() {
        unsafe {
            let distro = pk_get_distro_id();
            println!("distro {}", *distro);
        }
    }

    #[test]
    fn packagekit_id_check() {
        unsafe {
            let id = pk_package_id_build(
                std::ffi::CString::new("firefox").unwrap().as_ptr(),
                std::ffi::CString::new("140.0.4-367").unwrap().as_ptr(),
                std::ffi::CString::new("x86_64").unwrap().as_ptr(),
                std::ffi::CString::new("installed:Unstable")
                    .unwrap()
                    .as_ptr(),
            );

            let id_ok = pk_package_id_check(id);
            assert_eq!(1, id_ok);
        }
    }
}
