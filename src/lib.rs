#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::collections::BTreeSet;
use std::collections::HashSet;
use std::ffi::CStr;
use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::OnceLock;
use std::ptr;
use std::fmt;
use std::ffi::c_void;
use std::path::PathBuf;
use std::path::Path;
use std::sync::RwLock;

use ffi_convert::RawPointerConverter;
use fs_err::File;
use itertools::Itertools;
use moss::package::Id;
use moss::state::Selection;
use packagekit::PkErrorEnum_PK_ERROR_ENUM_FAILED_FINALISE;
use packagekit::PkErrorEnum_PK_ERROR_ENUM_PACKAGE_ALREADY_INSTALLED;
use packagekit::PkErrorEnum_PK_ERROR_ENUM_PACKAGE_ID_INVALID;
use packagekit::PkInfoEnum_PK_INFO_ENUM_INSTALL;
use packagekit::PkInfoEnum_PK_INFO_ENUM_INSTALLING;
use packagekit::PkInfoEnum_PK_INFO_ENUM_REMOVE;
use packagekit::PkInfoEnum_PK_INFO_ENUM_UPDATING;
use packagekit::PkStatusEnum_PK_STATUS_ENUM_DEP_RESOLVE;
use packagekit::PkStatusEnum_PK_STATUS_ENUM_DOWNLOAD;
use packagekit::PkStatusEnum_PK_STATUS_ENUM_INSTALL;
use packagekit::PkStatusEnum_PK_STATUS_ENUM_REMOVE;
use packagekit::PkStatusEnum_PK_STATUS_ENUM_UPDATE;
use packagekit::PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_ONLY_DOWNLOAD;
use packagekit::PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_SIMULATE;
use url::Url;
use ffi_convert::{CReprOf, CStringArray};

use glib_sys::g_variant_get;
use glib_sys::GVariant;
use glib_sys::{g_log, G_LOG_LEVEL_DEBUG};
use glib_sys::g_build_filename;

mod packagekit;
use packagekit::pk_backend_job_error_code;
use packagekit::pk_backend_job_repo_detail;
use packagekit::pk_backend_job_set_percentage;
use packagekit::pk_backend_job_set_status;
use packagekit::PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION;
use packagekit::PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND;
use packagekit::PkErrorEnum_PK_ERROR_ENUM_REPO_CONFIGURATION_ERROR;
use packagekit::PkErrorEnum_PK_ERROR_ENUM_REPO_NOT_FOUND;
use packagekit::PkStatusEnum_PK_STATUS_ENUM_REFRESH_CACHE;
use packagekit::PkErrorEnum_PK_ERROR_ENUM_NOT_SUPPORTED;
use packagekit::PkErrorEnum_PK_ERROR_ENUM_PACKAGE_DOWNLOAD_FAILED;
use packagekit::PkInfoEnum_PK_INFO_ENUM_DOWNGRADING;
use packagekit::PkInfoEnum_PK_INFO_ENUM_NORMAL;
use packagekit::PkRestartEnum_PK_RESTART_ENUM_NONE;
use packagekit::PkUpdateStateEnum_PK_UPDATE_STATE_ENUM_UNKNOWN;
use packagekit::_GVariant;
use packagekit::pk_backend_job_set_item_progress;
use packagekit::pk_backend_job_update_detail;
use packagekit::pk_backend_job_details;
use packagekit::pk_backend_job_details_full;
use packagekit::pk_backend_job_files;
use packagekit::pk_backend_job_thread_create;
use packagekit::pk_package_id_split;
use packagekit::PkGroupEnum_PK_GROUP_ENUM_UNKNOWN;
use packagekit::PK_PACKAGE_ID_NAME;
use packagekit::{PkBackend, PkBitfield, PkBackendJob, pk_backend_job_package, PkInfoEnum_PK_INFO_ENUM_AVAILABLE, PkInfoEnum_PK_INFO_ENUM_INSTALLED, pk_package_id_build, pk_backend_job_finished, GKeyFile};

use moss::package::{self, Name};
use moss::{client::{self, Client}, Installation, Repository, repository::{self, Priority}, runtime, environment, Provider, Package, package::Flags, registry::transaction};
use stone::payload::layout;
use stone::payload::meta;
use stone::read::PayloadKind;
use vfs::tree::BlitFile;

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
        let p = unsafe {*ptr.add(i)};
        if p.is_null() {
            break;
        }

        let cstr = unsafe {CStr::from_ptr(p)};
        match cstr.to_str() {
            Ok(s) => result.push(s.to_string()),
            Err(_) => (), // skip invalid UTF-8
        }

        i += 1;
    }

    result
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

// C Macros do not get translated to FFI
#[inline]
fn pk_bitfield_value(val: u32) -> PkBitfield {
    1 << val
}

#[inline]
fn pk_bitfield_contain(bitfield: PkBitfield, enum_val: u32) -> bool {
    (bitfield & pk_bitfield_value(enum_val)) > 0
}

fn get_moss_client() -> MossBackend {
    //let installation = Installation::open("/home/ninya/aeryn/img-tests/virt-manager-vm/sosroot/", None).expect("failed to open installation");
    let installation = Installation::open("/", None).expect("failed to open installation");
    let client = Client::new(environment::NAME, installation.clone()).expect("failed to create client");
    MossBackend { client, installation }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_initialize(_conf: GKeyFile, _backend: *mut PkBackend) -> () {
    log_debug!("HEY HO, WHAT IS UP FROM MOSS YO...");

    //let installation = Installation::open("/home/ninya/aeryn/img-tests/virt-manager-vm/sosroot/", None)
    //    .expect("failed to open installation");
    //let client = Client::new(environment::NAME, installation.clone())
    //    .expect("failed to create client");
    //BACKEND_CONTEXT
    //    .set(BackendContext { client, installation }).unwrap_or_else(|_| panic!("Failed to create client"));
}

struct MossBackend {
    client: Client,
    installation: Installation,
}

struct Output {
    name: Name,
    version: String,
    summary: String,
    arch: String,
    installed: bool,
    status: String,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pk_backend_destroy(_backend: *mut PkBackend) -> () {
    log_debug!("moss backend destroyed.");
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_description(_backend: *mut PkBackend) -> *const c_char {
    static DESCRIPTION: &str = "Moss - atomic stateful package manager\0";
    DESCRIPTION.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_author(_backend: *mut PkBackend) -> *const c_char {
    static AUTHOR: &str = "Joey Riches <johndoe@gmail.com>\0";
    AUTHOR.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_groups(_backend: *mut PkBackend) -> PkBitfield {
    return 0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_details_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let package_ids: *mut *const c_char = std::ptr::null_mut();
    let format = CString::new("(^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &package_ids); }

    let backend = get_moss_client();
    let client = &backend.client;

    let mut i = 0;
    unsafe {
        loop {
            let package_id = *package_ids.add(i);
            if package_id.is_null() {
                break;
            }

            let parts = pk_package_id_split(package_id);

            if !parts.is_null() {
                let name_ptr = *parts.add(PK_PACKAGE_ID_NAME as usize);

                if !name_ptr.is_null() {
                    let name_str = CStr::from_ptr(name_ptr).to_str().unwrap();
                    // FIXME: some helper function to convert package id to moss db package?
                    //        exact match version and arch as well
                    let res = client.registry.by_keyword(name_str, package::Flags::default())
                                                    .filter(|pkg| pkg.meta.name.to_string() == *name_str).next();
                    match res {
                        Some(res) => {
                            let c_sum = CString::new(res.meta.summary).unwrap();
                            let c_lic = CString::new(res.meta.licenses.first().unwrap().to_string()).unwrap();
                            let c_desc = CString::new(res.meta.description).unwrap();
                            let c_url = CString::new(res.meta.homepage).unwrap();
                            // FIXME: No way to get installed size of a package?
                            pk_backend_job_details_full(_job, package_id, c_sum.as_ptr(), c_lic.as_ptr(), PkGroupEnum_PK_GROUP_ENUM_UNKNOWN, c_desc.as_ptr(), c_url.as_ptr(), 0, res.meta.download_size.unwrap());
                        }
                        None => {
                            pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND, CString::new(format!("Failed to find package {:?}", package_id)).unwrap().as_ptr());
                        }
                    }
                }
            }
            i += 1;
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_details(_backend: *mut PkBackend, _job: *mut PkBackendJob, _package_ids: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_get_details_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_details_local_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let full_paths: *mut *const c_char = std::ptr::null_mut();
    let format = CString::new("(^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &full_paths); }

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

        for payload in payloads.flatten() {
            match payload {
                PayloadKind::Meta(meta) => {
                    for record in meta.body {
                        match &record.tag {
                            // HOLY MOTHER OF NESTING, BETTER WAY!?
                            meta::Tag::Name => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
                                        pkg_name = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            meta::Tag::Version => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
                                        pkg_arch = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            meta::Tag::Architecture => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
                                        pkg_ver = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            meta::Tag::Summary => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
                                        pkg_summary = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            meta::Tag::License => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
                                        pkg_license = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            meta::Tag::Description => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
                                        pkg_desc = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            meta::Tag::Homepage => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
                                        pkg_homepage = Some(s);
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
        let c_sum = CString::new(pkg_summary.unwrap().as_str()).unwrap();
        let c_desc = CString::new(pkg_desc.unwrap().as_str()).unwrap();
        let c_home = CString::new(pkg_homepage.unwrap().as_str()).unwrap();
        let c_lic = CString::new(pkg_license.unwrap().as_str()).unwrap();

        unsafe {
            let id = pk_package_id_build(c_name.as_ptr(),
                                c_ver.as_ptr(),
                                c_arch.as_ptr(),
                                CString::new(pkg_status).unwrap().as_ptr());
            pk_backend_job_details(_job,
                                    id,
                                    c_sum.as_ptr(),
                                    c_lic.as_ptr(),
                                    PkGroupEnum_PK_GROUP_ENUM_UNKNOWN,
                                    c_desc.as_ptr(),
                                    c_home.as_ptr(),
                                    0)
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_details_local(_backend: *mut PkBackend, _job: *mut PkBackendJob, _full_paths: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_get_details_local_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_packages_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let mut _filters: PkBitfield = 0;
    let format = CString::new("(t)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &_filters); }

    let backend = get_moss_client();
    let client = &backend.client;

    let pkgs = client.registry.list(package::Flags::default()).map(|pkg| Output {
                name: pkg.meta.name,
                version: pkg.meta.version_identifier,
                summary: pkg.meta.summary,
                arch: pkg.meta.architecture,
                installed: pkg.flags.installed,
                // FIXME: no way to get repo origin of package currently :(
                status: "volatile".to_string(),
            });

    for pkg in pkgs {
        unsafe {
            let mut c_status = CString::new(pkg.status.clone()).unwrap();
            if pkg.installed {
                c_status = CString::new(format!("{}:installed", pkg.status)).unwrap();
            }
            let id = pk_package_id_build(CString::new(pkg.name.to_string()).unwrap().as_ptr(),
                                         CString::new(pkg.version).unwrap().as_ptr(),
                                         CString::new(pkg.arch).unwrap().as_ptr(),
                                         c_status.as_ptr());
            if pkg.installed {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_INSTALLED, id, CString::new(pkg.summary).unwrap().as_ptr());
            } else {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_AVAILABLE, id, CString::new(pkg.summary).unwrap().as_ptr());
            }
        }
    }
}


#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_packages(_backend: *mut PkBackend, _job: *mut PkBackendJob, _filters: PkBitfield) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_get_packages_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_resolve_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let search: *mut *const c_char = std::ptr::null_mut();
    let filters: PkBitfield = 0;
    let format = CString::new("(t^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &filters, &search); }

    let backend = get_moss_client();
    let client = &backend.client;

    let search_terms = c_char_array_to_vec(search);

    let mut output = Vec::new();

    let mut seen = HashSet::new();
    for keyword in &search_terms {
        let matches: Vec<_> = client
            .registry
            .by_keyword(keyword, package::Flags::default())
            .filter(|pkg| pkg.meta.name.to_string() == *keyword)
            .sorted_by_key(|pkg| !pkg.flags.installed)
            .collect();

        // We have to filter out the remote versions of packages which
        // are already installed :(
        // TODO: however, for the newest filter we operate on available and installed
        //       lists separately
        for pkg in matches {
            if seen.insert(pkg.meta.name.to_string().clone()) {
                output.push(Output {
                    name: pkg.meta.name,
                    version: pkg.meta.version_identifier,
                    summary: pkg.meta.summary,
                    arch: pkg.meta.architecture,
                    installed: pkg.flags.installed,
                    // FIXME: no way to get repo origin of package currently :(
                    status: "volatile".to_string(),
                });
            }
        }
    }
    for pkg in output {
        unsafe {
            let mut c_status = CString::new(pkg.status.clone()).unwrap();
            if pkg.installed {
                c_status = CString::new(format!("{}:installed", pkg.status)).unwrap();
            }
            let id = pk_package_id_build(CString::new(pkg.name.to_string()).unwrap().as_ptr(),
                                         CString::new(pkg.version).unwrap().as_ptr(),
                                         CString::new(pkg.arch).unwrap().as_ptr(),
                                         c_status.as_ptr());
            if pkg.installed {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_INSTALLED, id, CString::new(pkg.summary).unwrap().as_ptr());
            } else {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_AVAILABLE, id, CString::new(pkg.summary).unwrap().as_ptr());
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_resolve(_backend: *mut PkBackend, _job: *mut PkBackendJob, _filters: PkBitfield, _search: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_resolve_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_files_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let package_ids: *mut *const c_char = std::ptr::null_mut();

    let format = CString::new("(^a&s)").unwrap();

    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;

    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &package_ids); }

    let backend = get_moss_client();
    let client = &backend.client;

    let mut i = 0;
    unsafe {
        loop {
            let package_id = *package_ids.add(i);
            if package_id.is_null() {
                break;
            }

            let parts = pk_package_id_split(package_id);

            if !parts.is_null() {
                let name_ptr = *parts.add(PK_PACKAGE_ID_NAME as usize);

                if !name_ptr.is_null() {
                    let name_str = CStr::from_ptr(name_ptr).to_str().unwrap();
                    // FIXME: some helper function to convert package id to moss db package?
                    //        exact match version and arch as well
                    let lookup = Provider::from_name(name_str).unwrap();
                    let resolved = client.registry.by_provider(&lookup, package::Flags::default()).unique_by(|p| p.id.clone()).next();
                    match resolved {
                        Some(s) => {
                            let vfs = client.vfs(&[s.id]).unwrap();
                            let files = vfs.iter().filter_map(|file| {
                                if matches!(file.kind(), vfs::tree::Kind::Directory) {
                                    return None;
                                }
                                let path = file.path();
                                Some(path)
                            }).collect::<Vec<_>>();

                            let mut files_ptr = OurCStringArray::from_vec(files);

                            pk_backend_job_files(_job, package_id, files_ptr.as_ptr());
                        }
                        None => {
                            pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND, CString::new(format!("Failed to find package {:?}", package_id)).unwrap().as_ptr());
                        }
                    }
                }
            }
            i += 1;
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_files(_backend: *mut PkBackend, _job: *mut PkBackendJob, _package_ids: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_get_files_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_files_local_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let full_paths: *mut *const c_char = std::ptr::null_mut();

    let format = CString::new("(^a&s)").unwrap();

    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;

    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &full_paths); }

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
                PayloadKind::Layout(l) => layouts = l.body,
                PayloadKind::Meta(meta) => {
                    for record in meta.body {
                        match &record.tag {
                            // HOLY MOTHER OF NESTING, BETTER WAY!?
                            meta::Tag::Name => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
                                        pkg_name = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            meta::Tag::Version => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
                                        pkg_arch = Some(s);
                                    }
                                    _ => {}
                                }
                            }
                            meta::Tag::Architecture => {
                                let kind = record.kind.clone();
                                match kind {
                                    meta::Kind::String(s) => {
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

        let files = layouts.iter().filter_map(|file| {
            match &file.entry {
                layout::Entry::Regular(_, target)
                | layout::Entry::Directory(target)
                | layout::Entry::Symlink(_, target) => {
                    Some(format!("/usr/{}", target.clone()))
                }
                _ => None,
            }
        }).collect::<Vec<_>>();

        let mut files_ptr = OurCStringArray::from_vec(files);

        unsafe {
            let id = pk_package_id_build(c_name.as_ptr(),
                                c_ver.as_ptr(),
                                c_arch.as_ptr(),
                                CString::new(pkg_status).unwrap().as_ptr());
            pk_backend_job_files(_job, id, files_ptr.as_ptr());
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_files_local(_backend: *mut PkBackend, _job: *mut PkBackendJob, _full_paths: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_get_files_local_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_search_files_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let search: *mut *const c_char = std::ptr::null_mut();
    let filters: PkBitfield = 0;
    let format = CString::new("(t^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &filters, &search); }

    let backend = get_moss_client();
    let client = &backend.client;

    let search_terms = c_char_array_to_vec(search);

    let mut output = Vec::new();

    let layouts = client.layout_db.all().unwrap();

    layouts.into_iter().for_each(|(id, layout)| match layout.entry {
        stone::payload::layout::Entry::Regular(_, file)
        | stone::payload::layout::Entry::Symlink(_, file)
        | stone::payload::layout::Entry::Directory(file) => {
            for keyword in &search_terms {
                if file.contains(keyword) {
                    if let Some(pkg) = client.registry.by_id(&id).next() {
                        let out = Output {
                            name: pkg.meta.name.clone(),
                            version: pkg.meta.version_identifier.clone(),
                            summary: pkg.meta.summary.clone(),
                            arch: pkg.meta.architecture.clone(),
                            status: "volatile".to_string(), // or dynamic based on your logic
                            installed: pkg.flags.installed,
                        };
                        output.push(out);
                    }
                }
            }
        }
        _ => {}
    });

    for pkg in output {
        unsafe {
            let mut c_status = CString::new(pkg.status.clone()).unwrap();
            if pkg.installed {
                c_status = CString::new(format!("{}:installed", pkg.status)).unwrap();
            }
            let id = pk_package_id_build(CString::new(pkg.name.to_string()).unwrap().as_ptr(),
                                         CString::new(pkg.version).unwrap().as_ptr(),
                                         CString::new(pkg.arch).unwrap().as_ptr(),
                                         c_status.as_ptr());
            if pkg.installed {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_INSTALLED, id, CString::new(pkg.summary).unwrap().as_ptr());
            } else {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_AVAILABLE, id, CString::new(pkg.summary).unwrap().as_ptr());
            }
        }
    }

}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_search_files(_backend: *mut PkBackend, _job: *mut PkBackendJob, _filters: PkBitfield, _values: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_search_files_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_search_details_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let search: *mut *const c_char = std::ptr::null_mut();
    let filters: PkBitfield = 0;
    let format = CString::new("(t^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &filters, &search); }

    let backend = get_moss_client();
    let client = &backend.client;

    let search_terms = c_char_array_to_vec(search);

    let mut output = Vec::new();

    for keyword in &search_terms {
        let matches = client
            .registry
            .list_available(Flags::default())
            .filter(|pkg| pkg.meta.summary.contains(keyword) || pkg.meta.description.contains(keyword))
            .map(|pkg| Output {
                name: pkg.meta.name,
                version: pkg.meta.version_identifier,
                summary: pkg.meta.summary,
                arch: pkg.meta.architecture,
                installed: pkg.flags.installed,
                // FIXME: no way to get repo origin of package currently :(
                status: "volatile".to_string(),
            });
        output.extend(matches);
    }
    for pkg in output {
        unsafe {
            let mut c_status = CString::new(pkg.status.clone()).unwrap();
            if pkg.installed {
                c_status = CString::new(format!("{}:installed", pkg.status)).unwrap();
            }
            let id = pk_package_id_build(CString::new(pkg.name.to_string()).unwrap().as_ptr(),
                                         CString::new(pkg.version).unwrap().as_ptr(),
                                         CString::new(pkg.arch).unwrap().as_ptr(),
                                         c_status.as_ptr());
            if pkg.installed {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_INSTALLED, id, CString::new(pkg.summary).unwrap().as_ptr());
            } else {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_AVAILABLE, id, CString::new(pkg.summary).unwrap().as_ptr());
            }
        }
    }

}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_search_details(_backend: *mut PkBackend, _job: *mut PkBackendJob, _filters: PkBitfield, _values: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_search_details_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_search_names_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let search: *mut *const c_char = std::ptr::null_mut();
    let filters: PkBitfield = 0;
    let format = CString::new("(t^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &filters, &search); }

    let backend = get_moss_client();
    let client = &backend.client;

    let search_terms = c_char_array_to_vec(search);

    let mut output = Vec::new();

    for keyword in &search_terms {
        let matches = client
            .registry
            .by_keyword(keyword, package::Flags::default())
            .map(|pkg| Output {
                name: pkg.meta.name,
                version: pkg.meta.version_identifier,
                summary: pkg.meta.summary,
                arch: pkg.meta.architecture,
                installed: pkg.flags.installed,
                // FIXME: no way to get repo origin of package currently :(
                status: "volatile".to_string(),
            });
        output.extend(matches);
    }
    for pkg in output {
        unsafe {
            let mut c_status = CString::new(pkg.status.clone()).unwrap();
            if pkg.installed {
                c_status = CString::new(format!("{}:installed", pkg.status)).unwrap();
            }
            let id = pk_package_id_build(CString::new(pkg.name.to_string()).unwrap().as_ptr(),
                                         CString::new(pkg.version).unwrap().as_ptr(),
                                         CString::new(pkg.arch).unwrap().as_ptr(),
                                         c_status.as_ptr());
            if pkg.installed {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_INSTALLED, id, CString::new(pkg.summary).unwrap().as_ptr());
            } else {
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_AVAILABLE, id, CString::new(pkg.summary).unwrap().as_ptr());
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_search_names(_backend: *mut PkBackend, _job: *mut PkBackendJob, _filters: PkBitfield, _values: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_search_names_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_remove_packages_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
//	PkBitfield transaction_flags = 0;
//	gboolean autoremove = false;
//	gboolean allow_deps = false;
//	gchar **package_ids;
//	g_variant_get(params, "(t^a&sbb)", &transaction_flags, &package_ids, &allow_deps, &autoremove);

    let package_ids: *mut *const c_char = std::ptr::null_mut();
    let transaction_flags: PkBitfield = 0;
    let allow_deps: i32 = 0;
    let autoremove: i32 = 0;
    let format = CString::new("(t^a&sbb)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &transaction_flags, &package_ids, &allow_deps, &autoremove); }

    let backend = get_moss_client();
    let client = &backend.client;

    let _guard = runtime::init();

    unsafe { pk_backend_job_set_status(_job, PkStatusEnum_PK_STATUS_ENUM_DEP_RESOLVE); }

    let mut resolved: Vec<Id> = Vec::new();

    // Firstly, let's resolve our pk package_ids back into moss pkgs
    let mut i = 0;
    unsafe {
        loop {
            let package_id = *package_ids.add(i);
            if package_id.is_null() {
                break;
            }

            let parts = pk_package_id_split(package_id);

            if !parts.is_null() {
                let name_ptr = *parts.add(PK_PACKAGE_ID_NAME as usize);
                if !name_ptr.is_null() {
                    let name_str = CStr::from_ptr(name_ptr).to_str().unwrap();

                    let res = client::install::find_packages(name_str, client);

                    if let (_, Some(pkg)) = res {
                        // NOTE: we need to re-resolve the pkg against the registry to get the full meta.uri
                        //       otherwise we just get the endix path missing the index base uri.
                        let full_pkg = client.registry.by_id(&pkg.id).next().unwrap();
                        resolved.push(full_pkg.id);
                    }
                }
            }
            i += 1;
        }
    }

    // Now, we'll create a moss transaction with our pkgs; this will pull in dependencies etc.
    let mut tx = client.registry.transaction(transaction::Lookup::InstalledOnly).unwrap();

    let installed = client.registry.list_installed(Flags::default()).collect::<Vec<_>>();
    let installed_ids = installed.iter().map(|p| p.id.clone()).collect::<BTreeSet<_>>();

    tx.add(installed_ids.clone().into_iter().collect()).unwrap();
    tx.remove(resolved);
    let finalized = tx.finalize().cloned().collect::<BTreeSet<_>>();
    let removed = client.resolve_packages(installed_ids.difference(&finalized)).unwrap();

    if pk_bitfield_contain(transaction_flags, PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_SIMULATE) {
        for pkg in removed {
            unsafe {
                let id = pk_package_id_build(CString::new(pkg.meta.name.to_string()).unwrap().as_ptr(),
                                         CString::new(pkg.meta.version_identifier.clone()).unwrap().as_ptr(),
                                         CString::new(pkg.meta.architecture.clone()).unwrap().as_ptr(),
                                         CString::new("volatile").unwrap().as_ptr());
                let c_summary = CString::new(pkg.meta.summary.clone()).unwrap();
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_REMOVE, id, c_summary.as_ptr());
            }
        }
        return
    }

    unsafe { pk_backend_job_set_status(_job, PkStatusEnum_PK_STATUS_ENUM_REMOVE); }

    let new_state_pkgs = {
        let previous_selections = match client.installation.active_state {
            Some(id) => client.state_db.get(id).unwrap().selections,
            None => vec![],
        };

        finalized
            .into_iter()
            .map(|id| {
                previous_selections
                    .iter()
                    .find(|s| s.package == id)
                    .cloned()
                    // Should be unreachable since new state from removal
                    // is always a subset of the previous state
                    .unwrap_or_else(|| {
                        eprintln!("Unreachable: previous selection not found during removal for package {id:?}, marking as not explicit");

                        Selection {
                            package: id,
                            explicit: false,
                            reason: None,
                        }
                    })
            })
            .collect::<Vec<_>>()
    };

    match client.new_state(&new_state_pkgs, "Remove") {
        Ok(_) => {}
        Err(e) => {
            let c_err = e.to_string();
            unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_FAILED_FINALISE, CString::new(c_err).unwrap().as_ptr()); }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_remove_packages(_backend: *mut PkBackend, _job: *mut PkBackendJob, _transaction_flags: PkBitfield, _package_ids: *const *const c_char, _allow_deps: i32, _autoremove: i32) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_remove_packages_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_install_packages_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let package_ids: *mut *const c_char = std::ptr::null_mut();
    let transaction_flags: PkBitfield = 0;
    let format = CString::new("(t^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &transaction_flags, &package_ids); }

    let backend = get_moss_client();
    let client = &backend.client;

    let _guard = runtime::init();

    unsafe { pk_backend_job_set_status(_job, PkStatusEnum_PK_STATUS_ENUM_DEP_RESOLVE); }

    let mut resolved: Vec<Id> = Vec::new();

    // Firstly, let's resolve our pk package_ids back into moss pkgs
    let mut i = 0;
    unsafe {
        loop {
            let package_id = *package_ids.add(i);
            if package_id.is_null() {
                break;
            }

            let parts = pk_package_id_split(package_id);

            if !parts.is_null() {
                let name_ptr = *parts.add(PK_PACKAGE_ID_NAME as usize);
                if !name_ptr.is_null() {
                    let name_str = CStr::from_ptr(name_ptr).to_str().unwrap();

                    let res = client::install::find_packages(name_str, client);

                    if let (_, Some(pkg)) = res {
                        // NOTE: we need to re-resolve the pkg against the registry to get the full meta.uri
                        //       otherwise we just get the endix path missing the index base uri.
                        let full_pkg = client.registry.by_id(&pkg.id).next().unwrap();
                        resolved.push(full_pkg.id);
                    }
                }
            }
            i += 1;
        }
    }

    // Now, we'll create a moss transaction with our pkgs; this will pull in dependencies etc.
    let mut tx = client.registry.transaction(transaction::Lookup::PreferInstalled).unwrap();

    match tx.add(resolved.clone()) {
        Ok(_) => {
            let tx_resolved = client.resolve_packages(tx.finalize()).unwrap();
            let installed = client.registry.list_installed(Flags::default()).collect::<Vec<_>>();
            let is_installed = |p: &Package| installed.iter().any(|i| i.meta.name == p.meta.name);
            let missing = tx_resolved
                .iter()
                .filter(|p| client.is_ephemeral() || !is_installed(p))
                .collect::<Vec<_>>();
            if missing.is_empty() {
                unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_ALREADY_INSTALLED, CString::new(format!("Package {:?} already installed", missing)).unwrap().as_ptr()); }
            }

            if pk_bitfield_contain(transaction_flags, PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_SIMULATE) {
                for pkg in missing.clone() {
                    unsafe {
                        let id = pk_package_id_build(CString::new(pkg.meta.name.to_string()).unwrap().as_ptr(),
                                                 CString::new(pkg.meta.version_identifier.clone()).unwrap().as_ptr(),
                                                 CString::new(pkg.meta.architecture.clone()).unwrap().as_ptr(),
                                                 CString::new("volatile").unwrap().as_ptr());
                        let c_summary = CString::new(pkg.meta.summary.clone()).unwrap();
                        pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_INSTALL, id, c_summary.as_ptr());
                    }
                }
                return
            }

            unsafe { pk_backend_job_set_status(_job, PkStatusEnum_PK_STATUS_ENUM_DOWNLOAD); }

            // TODO: progress callback see download_packages
            // TODO: Emit PK_INFO_ENUM_DOWNLOADING for each package.
            match runtime::block_on(async {client.cache_packages(&missing).await}) {
                Ok(_) => {}
                Err(e) => {
                    let c_err = e.to_string();
                    unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_DOWNLOAD_FAILED, CString::new(c_err).unwrap().as_ptr()); }
                }
            }
            let only_download = pk_bitfield_contain(transaction_flags, PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_ONLY_DOWNLOAD);
            if only_download {
                return
            }

            unsafe { pk_backend_job_set_status(_job, PkStatusEnum_PK_STATUS_ENUM_INSTALL); }

            // Finally, Let's fucking install the thing
            // TODO: we probably want callbacks here of what exactly we're installing
            //       we also need progress callbacks as well obviously
            let new_state_pkgs = {
                // Only use previous state in stateful mode
                let previous_selections = match client.installation.active_state {
                    Some(id) if !client.is_ephemeral() => client.state_db.get(id).unwrap().selections,
                    _ => vec![],
                };
                let missing_selections = missing.iter().map(|p| Selection {
                    package: p.id.clone(),
                    // Package is explicit if it was one of the input
                    // packages provided by the user
                    explicit: resolved.contains(&p.id),
                    reason: None,
                });

                missing_selections.chain(previous_selections).collect::<Vec<_>>()
            };

            // TODO: Emit PK_INFO_ENUM_INSTALLING for each package.
            match client.new_state(&new_state_pkgs, "Install") {
                Ok(_) => {
                }
                Err(e) => {
                    let c_err = e.to_string();
                    unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_FAILED_FINALISE, CString::new(c_err).unwrap().as_ptr()); }
                }
            }
        }
        Err(e) => {
            let c_err = CString::new(e.to_string()).unwrap();
            unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_ID_INVALID, CString::new(c_err).unwrap().as_ptr()); }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_install_packages(_backend: *mut PkBackend, _job: *mut PkBackendJob, _transaction_flags: PkBitfield, _package_ids: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_install_packages_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_update_packages_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let package_ids: *mut *const c_char = std::ptr::null_mut();
    let transaction_flags: PkBitfield = 0;
    let format = CString::new("(t^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &transaction_flags, &package_ids); }

    let backend = get_moss_client();
    let client = &backend.client;

    let _guard = runtime::init();

    unsafe { pk_backend_job_set_status(_job, PkStatusEnum_PK_STATUS_ENUM_DEP_RESOLVE); }

    let mut resolved: Vec<Id> = Vec::new();

    // Firstly, let's resolve our pk package_ids back into moss pkgs
    let mut i = 0;
    unsafe {
        loop {
            let package_id = *package_ids.add(i);
            if package_id.is_null() {
                break;
            }

            let parts = pk_package_id_split(package_id);

            if !parts.is_null() {
                let name_ptr = *parts.add(PK_PACKAGE_ID_NAME as usize);
                if !name_ptr.is_null() {
                    let name_str = CStr::from_ptr(name_ptr).to_str().unwrap();

                    let res = client::install::find_packages(name_str, client);

                    if let (_, Some(pkg)) = res {
                        // NOTE: we need to re-resolve the pkg against the registry to get the full meta.uri
                        //       otherwise we just get the endix path missing the index base uri.
                        let full_pkg = client.registry.by_id(&pkg.id).next().unwrap();
                        resolved.push(full_pkg.id);
                    }
                }
            }
            i += 1;
        }
    }

    let installed = client.registry.list_installed(package::Flags::default()).collect::<Vec<_>>();
    let all_ids = installed.iter().map(|p| &p.id).collect::<BTreeSet<_>>();

    let finalized = installed.iter().filter_map(|p| {
        if !p.flags.explicit {
            return None;
        }
        if let Some(lookup) = client.registry.by_name(&p.meta.name, package::Flags::new().with_available()).next() {
            if !all_ids.contains(&lookup.id) && lookup.meta.source_release > p.meta.source_release {
                return Some(lookup.id);
            }
        }
        Some(p.id.clone())
    }).collect::<Vec<_>>();

    let mut tx = client.registry.transaction(transaction::Lookup::PreferAvailable).unwrap();
    tx.add(finalized).unwrap();
    let synced = client.resolve_packages(tx.finalize()).unwrap();
    let final_fucking_sync = synced.iter().filter(|p| client.is_ephemeral() || !installed.iter().any(|i| i.id == p.id)).collect::<Vec<_>>();


    if pk_bitfield_contain(transaction_flags, PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_SIMULATE) {
        for pkg in final_fucking_sync.clone() {
            unsafe {
                let id = pk_package_id_build(CString::new(pkg.meta.name.to_string()).unwrap().as_ptr(),
                                         CString::new(pkg.meta.version_identifier.clone()).unwrap().as_ptr(),
                                         CString::new(pkg.meta.architecture.clone()).unwrap().as_ptr(),
                                         CString::new("volatile").unwrap().as_ptr());
                let c_summary = CString::new(pkg.meta.summary.clone()).unwrap();
                pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_UPDATING, id, c_summary.as_ptr());
            }
        }
        return
    }

    unsafe { pk_backend_job_set_status(_job, PkStatusEnum_PK_STATUS_ENUM_DOWNLOAD); }

    match runtime::block_on(async {client.cache_packages(&final_fucking_sync).await}) {
        Ok(_) => {}
        Err(e) => {
            let c_err = e.to_string();
            unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_DOWNLOAD_FAILED, CString::new(c_err).unwrap().as_ptr()); }
        }
    }
    let only_download = pk_bitfield_contain(transaction_flags, PkTransactionFlagEnum_PK_TRANSACTION_FLAG_ENUM_ONLY_DOWNLOAD);
    if only_download {
        return
    }

    unsafe { pk_backend_job_set_status(_job, PkStatusEnum_PK_STATUS_ENUM_UPDATE); }

    let new_selections = {
        let previous_selections = match client.installation.active_state {
            Some(id) => client.state_db.get(id).unwrap().selections,
            None => vec![],
        };

        synced
            .into_iter()
            .map(|p| {
                // Use old version id to lookup previous selection
                let lookup_id = installed
                    .iter()
                    .find_map(|i| (i.meta.name == p.meta.name).then_some(&i.id))
                    .unwrap_or(&p.id);

                previous_selections
                    .iter()
                    .find(|s| s.package == *lookup_id)
                    .cloned()
                    // Use prev reason / explicit flag & new id
                    .map(|s| Selection {
                        package: p.id.clone(),
                        ..s
                    })
                    // Must be transitive
                    .unwrap_or(Selection {
                        package: p.id,
                        explicit: false,
                        reason: None,
                    })
            })
            .collect::<Vec<_>>()
    };

    match client.new_state(&new_selections, "Sync") {
        Ok(_) => {}
        Err(e) => {
            let c_err = e.to_string();
            unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_FAILED_FINALISE, CString::new(c_err).unwrap().as_ptr()); }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_update_packages(_backend: *mut PkBackend, _job: *mut PkBackendJob, _transaction_flags: PkBitfield, _package_ids: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_update_packages_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_download_packages_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let package_ids: *mut *const c_char = std::ptr::null_mut();
    let directory: *const c_char = std::ptr::null();
    let format = CString::new("(^a&ss)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &package_ids, &directory); }

    let backend = get_moss_client();
    let client = &backend.client;

    let _guard = runtime::init();

    let mut i = 0;
    unsafe {
        loop {
            let package_id = *package_ids.add(i);
            if package_id.is_null() {
                break;
            }

            let parts = pk_package_id_split(package_id);

            if !parts.is_null() {
                let name_ptr = *parts.add(PK_PACKAGE_ID_NAME as usize);

                if !name_ptr.is_null() {
                    let name_str = CStr::from_ptr(name_ptr).to_str().unwrap();
                    // FIXME: some helper function to convert package id to moss db package?
                    //        exact match version and arch as well
                    //let res = client.registry.by_keyword(name_str, package::Flags::default())
                    //                                .filter(|pkg| pkg.meta.name.to_string() == *name_str).next();

                    let res = client::install::find_packages(name_str, client);

                    if let (_, Some(pkg)) = res {
                        // NOTE: we need to re-resolve the pkg against the registry to get the full meta.uri
                        //       otherwise we just get the endix path missing the index base uri.
                        let full_pkg = client.registry.by_id(&pkg.id).next().unwrap();
                        match runtime::block_on(async { client::cache::fetch(&full_pkg.meta, &backend.installation, {
                            let package_id = package_id.clone();
                            //log_debug!("Attempting to download {:?}", full_pkg.meta.uri);
                            move |progress: moss::client::cache::Progress| {
                                let item_percentage = progress.pct();
                                let pk_percentage = (item_percentage * 100.0).floor() as u32;
                                //log_debug!("progress debugging {} {} {} {}", progress.completed, progress.total, item_percentage, pk_percentage);
                                pk_backend_job_set_item_progress(_job, package_id, PkInfoEnum_PK_INFO_ENUM_DOWNGRADING, pk_percentage);
                            }
                        }).await}) {
                            Ok(download) => {
                                // God this is horrible
                                let rust_dir = Path::new(CStr::from_ptr(directory).to_str().unwrap());

                                let target = rust_dir.join(&download.path.file_name().unwrap());

                                // TODO: rename to friendly file e.g. foo-1.2.3-x86_64.stone instead of file hash
                                //       it may be better to add various download helpers APIs in moss itself
                                // log_debug!("WHAT IS OUR DOWNLOAD TARGET {target:?}");
                                std::fs::rename(download.path, &target).unwrap();

                                let testing = target.as_path().to_str().unwrap();

                                let target_vec = vec![testing.to_string()];
                                // this crate gives us *const *const i8 when we need *mut *mut i8, casting segfaults
                                //let mut c_target = CStringArray::c_repr_of(target_vec).expect("couldn't convert");
                                let mut c_target = OurCStringArray::from_vec(target_vec);

                                pk_backend_job_files(_job, package_id, c_target.as_ptr());
                            }
                            Err(e) => {
                                let c_err = CString::new(e.to_string()).unwrap();
                                pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_DOWNLOAD_FAILED, CString::new(c_err).unwrap().as_ptr());
                            }
                        }
                    } else {
                        pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND, CString::new(format!("Failed to find package {:?}", package_id)).unwrap().as_ptr());
                    }
                }
            }
            i += 1;
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_download_packages(_backend: *mut PkBackend, _job: *mut PkBackendJob, _package_ids: *const *const c_char, _directory: *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_download_packages_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_update_detail_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let package_ids: *mut *const c_char = std::ptr::null_mut();
    let format = CString::new("(^a&s)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &package_ids); }

    let backend = get_moss_client();
    let client = &backend.client;

    let mut i = 0;
    unsafe {
        loop {
            let package_id = *package_ids.add(i);
            if package_id.is_null() {
                break;
            }

            let parts = pk_package_id_split(package_id);

            if !parts.is_null() {
                let name_ptr = *parts.add(PK_PACKAGE_ID_NAME as usize);

                if !name_ptr.is_null() {
                    let name_str = CStr::from_ptr(name_ptr).to_str().unwrap();
                    // FIXME: some helper function to convert package id to moss db package?
                    //        exact match version and arch as well
                    let res = client.registry.by_keyword(name_str, package::Flags::default())
                                                    .filter(|pkg| pkg.meta.name.to_string() == *name_str).next();
                    match res {
                        Some(_) => {
                            // FIXME: Holy fuck we have like no information
                            pk_backend_job_update_detail(_job,
                                                         package_id,
                                                         ptr::null_mut(), // updates
                                                         ptr::null_mut(), // obsoletes
                                                         ptr::null_mut(), // vendor urls
                                                         ptr::null_mut(), // bugzilla urls
                                                         ptr::null_mut(), // cve urls
                                                         PkRestartEnum_PK_RESTART_ENUM_NONE, // package warrants restart?
                                                         ptr::null_mut(), // update text
                                                         ptr::null_mut(), // changelog
                                                         PkUpdateStateEnum_PK_UPDATE_STATE_ENUM_UNKNOWN, // update state
                                                         ptr::null_mut(), // issued (date)
                                                         ptr::null_mut()); // updated (date)
                        }
                        None => {
                            pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_PACKAGE_NOT_FOUND, CString::new(format!("Failed to find package {:?}", package_id)).unwrap().as_ptr());
                        }
                    }
                }
            }
            i += 1;
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_update_detail(_backend: *mut PkBackend, _job: *mut PkBackendJob, _package_ids: *const *const c_char) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_get_update_detail_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_get_updates_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let mut _filters: PkBitfield = 0;
    let format = CString::new("(t)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &_filters); }

    let backend = get_moss_client();
    let client = &backend.client;

    let pkgs_installed = client.registry.list(Flags::new().with_installed()).collect::<Vec<_>>();
    let pkgs_available = client.registry.list(Flags::new().with_available()).collect::<Vec<_>>();

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
                .map(|u| Output {
                    name: u.meta.name.clone(),
                    version: u.meta.version_identifier.clone(),
                    summary: u.meta.summary.clone(),
                    arch: u.meta.architecture.clone(),
                    installed: u.flags.installed,
                    // FIXME: no way to get repo origin of pkg currently :(
                    status: "volatile".to_string(),
                })
        })
        .collect::<Vec<_>>();
    set.sort_by_key(|s| s.name.clone());
    set.dedup_by_key(|s| s.name.clone());

    for pkg in set {
        unsafe {
            let id = pk_package_id_build(CString::new(pkg.name.to_string()).unwrap().as_ptr(),
                                         CString::new(pkg.version).unwrap().as_ptr(),
                                         CString::new(pkg.arch).unwrap().as_ptr(),
                                         CString::new(pkg.status).unwrap().as_ptr());
            let c_summary = CString::new(pkg.summary).unwrap();
            // TODO: no way to determine pkgs which are security fixes, other enum types are also available
            pk_backend_job_package(_job, PkInfoEnum_PK_INFO_ENUM_NORMAL, id, c_summary.as_ptr());
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_updates(_backend: *mut PkBackend, _job: *mut PkBackendJob, _filters: PkBitfield) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_get_updates_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn backend_refresh_cache_thread(_job: *mut PkBackendJob, params: *mut _GVariant, _user_data: *mut c_void) -> () {
    let mut _force: i32 = 0;
    let format = CString::new("(b)").unwrap();
    // Cast _GVariant to GVariant
    let gvariant_ptr = params as *mut GVariant;
    unsafe { g_variant_get(gvariant_ptr, format.as_ptr(), &_force); }

    let backend = get_moss_client();
    let config = config::Manager::system(&backend.installation.root, "moss");

    unsafe {
        pk_backend_job_set_status(_job, PkStatusEnum_PK_STATUS_ENUM_REFRESH_CACHE);
    }

    let _guard = runtime::init();

    match repository::Manager::system(config, backend.installation.clone()) {
        Ok(mut manager) => {
            let repo_len = manager.list().len();
            let mut idx = 0;
            match runtime::block_on(async {manager.refresh_all().await }) {
                Ok(_) => {
                    // FIXME: this doesn't do anything cause we await DUH
                    idx = idx + 1;
                    let percentage = if idx >= repo_len {
                        100
                    } else {
                        (100 * idx) / repo_len
                    } as u32;
                    unsafe { pk_backend_job_set_percentage(_job, percentage); }
                }
                Err(e) => {
                    let c_err = e.to_string();
                    unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_REPO_CONFIGURATION_ERROR, CString::new(c_err).unwrap().as_ptr()); }
                }
            }
        }
        Err(e) => {
            let c_err = e.to_string();
            unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION, CString::new(c_err).unwrap().as_ptr()); }
        }
    }
    unsafe { pk_backend_job_set_percentage(_job, 100); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_refresh_cache(_backend: *mut PkBackend, _job: *mut PkBackendJob, _force: i32) -> () {
    unsafe { pk_backend_job_thread_create(_job, Some(backend_refresh_cache_thread), ptr::null_mut(), None); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_repo_enable(_backend: *mut PkBackend, _job: *mut PkBackendJob, rid: *const c_char, enabled: i32) -> () {
    let backend = get_moss_client();
    let config = config::Manager::system(&backend.installation.root, "moss");

    unsafe {
        let c_str = CStr::from_ptr(rid);
        let rid_str = match c_str.to_str() {
            Ok(s) => s,
            Err(_) => {
                return
            }
        };

        let pk_id = repository::Id::new(&rid_str);
        let enabled: bool = enabled != 0;

        let _guard = runtime::init();

        match repository::Manager::system(config, backend.installation.clone()) {
            Ok(mut manager) => {
                // NOTE: borrowing issues with mutable vs immutable manager
                let repo_ids: Vec<_> = manager.list().map(|(id, repo)| (id.clone(), repo.clone())).collect();
                let mut found_repo = false;
                for (id, _repo) in repo_ids {
                    if id.clone() == pk_id {
                        found_repo = true;
                        if enabled {
                            match runtime::block_on(manager.enable(&id)) {
                                Ok(_) => {}
                                Err(e) => {
                                    let c_err = e.to_string();
                                    pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_REPO_CONFIGURATION_ERROR, CString::new(c_err).unwrap().as_ptr());
                                }
                            }
                        } else {
                            match runtime::block_on(manager.disable(&id)) {
                                Ok(_) => {}
                                Err(e) => {
                                    let c_err = e.to_string();
                                    pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_REPO_CONFIGURATION_ERROR, CString::new(c_err).unwrap().as_ptr());
                                }
                            }
                        }
                    }
                }
                if found_repo == false {
                    let c_err = CString::new(format!("Failed to find repo: {}", pk_id)).unwrap();
                    pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_REPO_NOT_FOUND, c_err.as_ptr());
                }
            }
            Err(e) => {
                let c_err = e.to_string();
                pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION, CString::new(c_err).unwrap().as_ptr());
            }
        }
    }
    unsafe { pk_backend_job_finished(_job); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_repo_set_data(_backend: *mut PkBackend, _job: *mut PkBackendJob, repo_id: *const c_char, parameter: *const c_char, value: *const c_char) -> () {
    let backend = get_moss_client();
    let config = config::Manager::system(&backend.installation.root, "moss");

    match repository::Manager::system(config, backend.installation.clone()) {
        Ok(mut manager) => {
            unsafe {
                let c_rid = CStr::from_ptr(repo_id);
                let rid_str = match c_rid.to_str() {
                    Ok(s) => s,
                    Err(_) => {
                        return
                    }
                };
                let c_param = CStr::from_ptr(parameter);
                let param_str = match c_param.to_str() {
                    Ok(s) => s,
                    Err(_) => {
                        return
                    }
                };

                let c_value = CStr::from_ptr(value);
                let value_str = match c_value.to_str() {
                    Ok(s) => s,
                    Err(_) => {
                        return
                    }
                };

                let pk_id = repository::Id::new(&rid_str);

                match param_str {
                    "add" => {
                        let uri = Url::parse(value_str).unwrap();
                        manager.add_repository(
                            pk_id.clone(),
                            Repository {
                                description: "...".to_string(),
                                uri,
                                priority: Priority::new(0),
                                active: true,
                            }).unwrap();
                        // TODO: should we actually refresh the repo here or rely on refresh-cache?
                        let _guard = runtime::init();
                        match runtime::block_on(manager.refresh(&pk_id)) {
                            Ok(_) => {}
                            Err(e) => {
                                let c_err = CString::new(e.to_string()).unwrap();
                                pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_REPO_NOT_FOUND, c_err.as_ptr());
                            }
                        }
                    }
                    "remove" => {
                        match manager.remove(pk_id.clone()).unwrap() {
                            repository::manager::Removal::NotFound => {
                                let c_err = CString::new(format!("Repository id: {} was not found", pk_id)).unwrap();
                                pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_REPO_NOT_FOUND, c_err.as_ptr());
                            }
                            repository::manager::Removal::ConfigDeleted(false) => {}
                            repository::manager::Removal::ConfigDeleted(true) => {}
                        }
                    }
                    // TODO: modify priority and url of existing repos?
                    _ => {
                        let c_err = CString::new("Valid parameters for set_repo_data are: add and, remove").unwrap();
                        pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_NOT_SUPPORTED, c_err.as_ptr());
                    }
                }
            }
        }
        Err(e) => {
            let c_err = e.to_string();
            unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION, CString::new(c_err).unwrap().as_ptr()); }
        }
    }
    unsafe { pk_backend_job_finished(_job); }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_repo_list(_backend: *mut PkBackend, _job: *mut PkBackendJob, _filters: PkBitfield) -> () {
    let backend = get_moss_client();
    let config = config::Manager::system(&backend.installation.root, "moss");

    match repository::Manager::system(config, backend.installation.clone()) {
        Ok(manager) => {
            let configured_repos = manager.list();
            if configured_repos.len() == 0 {
                // TODO, set pk_backend_job_error_code?
                return;
            }
            for (id, repo) in configured_repos.sorted_by(|(_, a), (_, b)| a.priority.cmp(&b.priority).reverse()) {
                let c_id = CString::new(id.to_string()).unwrap();
                let c_desc = CString::new(repo.description.clone()).unwrap();
                let c_active = if repo.active { 1 } else { 0 };
                unsafe { pk_backend_job_repo_detail(_job, c_id.as_ptr(), c_desc.as_ptr(), c_active); }
            }
        }
        Err(e) => {
            let c_err = e.to_string();
            unsafe { pk_backend_job_error_code(_job, PkErrorEnum_PK_ERROR_ENUM_FAILED_INITIALIZATION, CString::new(c_err).unwrap().as_ptr()); }
        }
    }
    unsafe { pk_backend_job_finished(_job); }
}

unsafe fn c_strings_to_vec_null_terminated(mut c_strings: *const *const c_char) -> Vec<String> {
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
    pub fn from_vec(strings: Vec<String>) -> Self {
        let cstrings: Vec<CString> = strings
            .into_iter()
            .map(|s| CString::new(s).unwrap())
            .collect();

        // Create vector of raw pointers
        let mut ptrs: Vec<*mut c_char> = cstrings
            .iter()
            .map(|cs| cs.as_ptr() as *mut c_char)
            .collect();

        ptrs.push(ptr::null_mut()); // Null terminate

        OurCStringArray { cstrings, ptrs }
    }

    /// Returns the pointer to the first pointer (char **)
    pub fn as_ptr(&mut self) -> *mut *mut c_char {
        self.ptrs.as_mut_ptr()
    }
}

// This function is LLM slop i have no fucking idea how to do this nicely
#[unsafe(no_mangle)]
unsafe extern "C" fn pk_backend_get_mime_types(_backend: *mut PkBackend) -> *mut *mut c_char {
    let mime_types = vec![
        CString::new("application/x-stone-binary").unwrap(),
    ];

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
            let id = pk_package_id_build(std::ffi::CString::new("firefox").unwrap().as_ptr(),
                     std::ffi::CString::new("140.0.4-367").unwrap().as_ptr(),
                     std::ffi::CString::new("x86_64").unwrap().as_ptr(),
                     std::ffi::CString::new("installed:Unstable").unwrap().as_ptr());

            let id_ok = pk_package_id_check(id);
            assert_eq!(1, id_ok);
        }
    }
}
