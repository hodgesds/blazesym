#![allow(clippy::let_and_return, clippy::let_unit_value)]

use std::ffi::CStr;
use std::ffi::CString;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr;
use std::slice;

use blazesym::inspect;

use blazesym::c_api::blaze_inspect_elf_src;
use blazesym::c_api::blaze_inspect_syms_elf;
use blazesym::c_api::blaze_inspector_free;
use blazesym::c_api::blaze_inspector_new;
use blazesym::c_api::blaze_normalize_user_addrs;
use blazesym::c_api::blaze_normalize_user_addrs_sorted;
use blazesym::c_api::blaze_normalizer_free;
use blazesym::c_api::blaze_normalizer_new;
use blazesym::c_api::blaze_symbolize;
use blazesym::c_api::blaze_symbolizer_free;
use blazesym::c_api::blaze_symbolizer_new;
use blazesym::c_api::blaze_symbolizer_new_opts;
use blazesym::c_api::blaze_symbolizer_opts;
use blazesym::c_api::blaze_syms_free;
use blazesym::c_api::blaze_user_addrs_free;
use blazesym::c_api::blazesym_result_free;
use blazesym::c_api::blazesym_src_type;
use blazesym::c_api::blazesym_ssc_elf;
use blazesym::c_api::blazesym_ssc_gsym;
use blazesym::c_api::blazesym_ssc_params;
use blazesym::c_api::blazesym_sym_src_cfg;
use blazesym::Addr;


/// Make sure that we can create and free a symbolizer instance.
#[test]
fn symbolizer_creation() {
    let symbolizer = blaze_symbolizer_new();
    let () = unsafe { blaze_symbolizer_free(symbolizer) };
}


/// Make sure that we can create and free a symbolizer instance with the
/// provided options.
#[test]
fn symbolizer_creation_with_opts() {
    let opts = blaze_symbolizer_opts {
        debug_syms: true,
        src_location: false,
    };
    let symbolizer = unsafe { blaze_symbolizer_new_opts(&opts) };
    let () = unsafe { blaze_symbolizer_free(symbolizer) };
}


/// Make sure that we can symbolize an address.
#[test]
fn symbolize_from_file() {
    fn test(src: blazesym_sym_src_cfg) {
        let symbolizer = blaze_symbolizer_new();
        let addrs = [0x2000100];
        let result = unsafe { blaze_symbolize(symbolizer, &src, addrs.as_ptr(), addrs.len()) };

        assert!(!result.is_null());

        let result = unsafe { &*result };
        assert_eq!(result.size, 1);
        let entries = unsafe { slice::from_raw_parts(result.entries.as_ptr(), result.size) };
        let entry = &entries[0];
        assert_eq!(entry.size, 1);

        let syms = unsafe { slice::from_raw_parts(entry.syms, entry.size) };
        let sym = &syms[0];
        assert_eq!(
            unsafe { CStr::from_ptr(sym.symbol) },
            CStr::from_bytes_with_nul(b"factorial\0").unwrap()
        );

        let () = unsafe { blazesym_result_free(result) };
        let () = unsafe { blaze_symbolizer_free(symbolizer) };
    }

    let test_dwarf = Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("test-dwarf.bin");
    let test_dwarf_c = CString::new(test_dwarf.to_str().unwrap()).unwrap();

    let elf_src = ManuallyDrop::new(blazesym_ssc_elf {
        path: test_dwarf_c.as_ptr(),
        base_address: 0,
    });
    let src = blazesym_sym_src_cfg {
        src_type: blazesym_src_type::BLAZESYM_SRC_T_ELF,
        params: blazesym_ssc_params { elf: elf_src },
    };
    test(src);

    let test_gsym = Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("test.gsym");
    let test_gsym_c = CString::new(test_gsym.to_str().unwrap()).unwrap();
    let gsym_src = ManuallyDrop::new(blazesym_ssc_gsym {
        path: test_gsym_c.as_ptr(),
        base_address: 0,
    });
    let src = blazesym_sym_src_cfg {
        src_type: blazesym_src_type::BLAZESYM_SRC_T_GSYM,
        params: blazesym_ssc_params { gsym: gsym_src },
    };
    test(src);
}


/// Make sure that we can create and free a normalizer instance.
#[test]
fn normalizer_creation() {
    let normalizer = blaze_normalizer_new();
    let () = unsafe { blaze_normalizer_free(normalizer) };
}


/// Check that we can normalize user space addresses.
#[test]
fn normalize_user_addrs() {
    let addrs = [
        libc::__errno_location as Addr,
        libc::dlopen as Addr,
        libc::fopen as Addr,
        lookup_dwarf as Addr,
        normalize_user_addrs as Addr,
    ];

    let normalizer = blaze_normalizer_new();
    assert_ne!(normalizer, ptr::null_mut());

    let result = unsafe {
        blaze_normalize_user_addrs(normalizer, addrs.as_slice().as_ptr(), addrs.len(), 0)
    };
    assert_ne!(result, ptr::null_mut());

    let user_addrs = unsafe { &*result };
    assert_eq!(user_addrs.meta_count, 2);
    assert_eq!(user_addrs.addr_count, 5);

    let () = unsafe { blaze_user_addrs_free(result) };
    let () = unsafe { blaze_normalizer_free(normalizer) };
}


/// Check that we can normalize sorted user space addresses.
#[test]
fn normalize_user_addrs_sorted() {
    let mut addrs = [
        libc::__errno_location as Addr,
        libc::dlopen as Addr,
        libc::fopen as Addr,
        lookup_dwarf as Addr,
        normalize_user_addrs as Addr,
    ];
    let () = addrs.sort();

    let normalizer = blaze_normalizer_new();
    assert_ne!(normalizer, ptr::null_mut());

    let result = unsafe {
        blaze_normalize_user_addrs_sorted(normalizer, addrs.as_slice().as_ptr(), addrs.len(), 0)
    };
    assert_ne!(result, ptr::null_mut());

    let user_addrs = unsafe { &*result };
    assert_eq!(user_addrs.meta_count, 2);
    assert_eq!(user_addrs.addr_count, 5);

    let () = unsafe { blaze_user_addrs_free(result) };
    let () = unsafe { blaze_normalizer_free(normalizer) };
}


/// Make sure that we can create and free an inspector instance.
#[test]
fn inspector_creation() {
    let inspector = blaze_inspector_new();
    let () = unsafe { blaze_inspector_free(inspector) };
}


/// Make sure that we can lookup a function's address using DWARF information.
#[test]
fn lookup_dwarf() {
    let test_dwarf = Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("test-dwarf.bin");

    let src = blaze_inspect_elf_src::from(inspect::Elf::new(test_dwarf));
    let factorial = CString::new("factorial").unwrap();
    let names = [factorial.as_ptr()];

    let inspector = blaze_inspector_new();
    let result = unsafe { blaze_inspect_syms_elf(inspector, &src, names.as_ptr(), names.len()) };
    let _src = inspect::Elf::from(src);

    let sym_infos = unsafe { slice::from_raw_parts(result, names.len()) };
    let sym_info = unsafe { &*sym_infos[0] };
    assert_eq!(
        unsafe { CStr::from_ptr(sym_info.name) },
        CStr::from_bytes_with_nul(b"factorial\0").unwrap()
    );
    assert_eq!(sym_info.address, 0x2000100);

    let () = unsafe { blaze_syms_free(result) };
    let () = unsafe { blaze_inspector_free(inspector) };
}
