use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

use crate::elf::ElfBackend;
use crate::elf::ElfCache;
use crate::elf::ElfResolver;
use crate::gsym::GsymResolver;
use crate::kernel::KernelResolver;
use crate::ksym::KSymCache;
use crate::ksym::KALLSYMS;
use crate::log;
use crate::maps;
use crate::maps::PathMapsEntry;
use crate::normalize;
use crate::normalize::create_apk_elf_path;
use crate::normalize::normalize_apk_addr;
use crate::normalize::normalize_elf_addr;
use crate::normalize::normalize_sorted_user_addrs_with_entries;
use crate::util;
use crate::util::uname_release;
use crate::Addr;
use crate::ErrorExt as _;
use crate::IntSym;
use crate::Pid;
use crate::Result;
use crate::SrcLang;
use crate::SymResolver;

use super::source::Elf;
use super::source::Gsym;
use super::source::GsymData;
use super::source::GsymFile;
use super::source::Kernel;
use super::source::Process;
use super::source::Source;


/// Demangle a symbol name using the demangling scheme for the given language.
#[cfg(feature = "demangle")]
fn maybe_demangle(name: &str, language: SrcLang) -> String {
    match language {
        SrcLang::Rust => rustc_demangle::try_demangle(name)
            .ok()
            .as_ref()
            .map(|x| format!("{x:#}")),
        SrcLang::Cpp => cpp_demangle::Symbol::new(name)
            .ok()
            .and_then(|x| x.demangle(&Default::default()).ok()),
        SrcLang::Unknown => rustc_demangle::try_demangle(name)
            .map(|x| format!("{x:#}"))
            .ok()
            .or_else(|| {
                cpp_demangle::Symbol::new(name)
                    .ok()
                    .and_then(|sym| sym.demangle(&Default::default()).ok())
            }),
    }
    .unwrap_or_else(|| name.to_string())
}

#[cfg(not(feature = "demangle"))]
fn maybe_demangle(name: &str, _language: SrcLang) -> String {
    // Demangling is disabled.
    name.to_string()
}


/// The result of symbolization by [`Symbolizer`].
#[derive(Clone, Debug)]
pub struct Sym {
    /// The symbol name that an address may belong to.
    pub name: String,
    /// The address at which the symbol is located (i.e., its "start").
    ///
    /// This is the "normalized" address of the symbol, as present in
    /// the file (and reported by tools such as `readelf(1)`,
    /// `llvm-gsymutil`, or similar).
    pub addr: Addr,
    /// The byte offset of the address that got symbolized from the
    /// start of the symbol (i.e., from `addr`).
    ///
    /// E.g., when normalizing address 0x1337 of a function that starts at
    /// 0x1330, the offset will be set to 0x07 (and `addr` will be 0x1330). This
    /// member is especially useful in contexts when input addresses are not
    /// already normalized, such as when normalizing an address in a process
    /// context (which may have been relocated and/or have layout randomizations
    /// applied).
    pub offset: usize,
    /// The directory in which the source file resides.
    pub dir: Option<PathBuf>,
    /// The file that defines the symbol.
    pub file: Option<OsString>,
    /// The line number of the symbolized instruction in the source
    /// code.
    ///
    /// This is the line number of the instruction of the address being
    /// symbolized, not the line number that defines the symbol
    /// (function).
    pub line: Option<u32>,
    /// The column number of the symbolized instruction in the source
    /// code.
    pub column: Option<u16>,
    /// The struct is non-exhaustive and open to extension.
    pub(crate) _non_exhaustive: (),
}


/// A builder for configurable construction of [`Symbolizer`] objects.
///
/// By default all features are enabled.
#[derive(Clone, Debug)]
pub struct Builder {
    /// Whether to enable usage of debug symbols.
    debug_syms: bool,
    /// Whether to attempt to gather source code location information.
    ///
    /// This setting implies usage of debug symbols and forces the corresponding
    /// flag to `true`.
    src_location: bool,
    /// Whether or not to transparently demangle symbols.
    ///
    /// Demangling happens on a best-effort basis. Currently supported
    /// languages are Rust and C++ and the flag will have no effect if
    /// the underlying language does not mangle symbols (such as C).
    demangle: bool,
}

impl Builder {
    /// Enable/disable line number information.
    pub fn enable_src_location(mut self, enable: bool) -> Builder {
        self.src_location = enable;
        self
    }

    /// Enable/disable usage of debug symbols.
    ///
    /// That can be useful in cases where ELF symbol information is stripped.
    pub fn enable_debug_syms(mut self, enable: bool) -> Builder {
        self.debug_syms = enable;
        self
    }

    /// Enable/disable usage of debug symbols.
    ///
    /// That can be useful in cases where ELF symbol information is stripped.
    pub fn enable_demangling(mut self, enable: bool) -> Builder {
        self.demangle = enable;
        self
    }

    /// Create the [`Symbolizer`] object.
    pub fn build(self) -> Symbolizer {
        let Builder {
            debug_syms,
            src_location,
            demangle,
        } = self;
        let ksym_cache = KSymCache::new();
        let elf_cache = ElfCache::new(src_location, debug_syms);

        Symbolizer {
            ksym_cache,
            elf_cache,
            src_location,
            demangle,
        }
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            src_location: true,
            debug_syms: true,
            demangle: true,
        }
    }
}


/// Symbolizer provides an interface to symbolize addresses.
#[derive(Debug)]
pub struct Symbolizer {
    ksym_cache: KSymCache,
    elf_cache: ElfCache,
    src_location: bool,
    demangle: bool,
}

impl Symbolizer {
    /// Create a new [`Symbolizer`].
    pub fn new() -> Self {
        Builder::default().build()
    }

    /// Retrieve a [`Builder`] object for configurable construction of a
    /// [`Symbolizer`].
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// Demangle the provided symbol if asked for and possible.
    fn maybe_demangle(&self, symbol: &str, language: SrcLang) -> String {
        if self.demangle {
            maybe_demangle(symbol, language)
        } else {
            symbol.to_string()
        }
    }

    /// Symbolize an address using the provided [`SymResolver`].
    #[cfg_attr(feature = "tracing", crate::log::instrument(skip_all, fields(addr = format_args!("{addr:#x}"), resolver = ?resolver)))]
    fn symbolize_with_resolver(&self, addr: Addr, resolver: &dyn SymResolver) -> Result<Vec<Sym>> {
        let syms = resolver.find_syms(addr)?;
        if syms.is_empty() {
            return Ok(Vec::new())
        }

        let linfo = if self.src_location {
            resolver.find_line_info(addr)?
        } else {
            None
        };

        let mut results = vec![];
        for sym in syms {
            if let Some(ref linfo) = linfo {
                let IntSym {
                    name,
                    addr: sym_addr,
                    lang,
                } = sym;
                results.push(Sym {
                    name: self.maybe_demangle(name, lang),
                    addr: sym_addr,
                    offset: addr - sym_addr,
                    dir: Some(linfo.dir.to_path_buf()),
                    file: Some(linfo.file.to_os_string()),
                    line: linfo.line,
                    column: linfo.column,
                    _non_exhaustive: (),
                });
            } else {
                let IntSym {
                    name,
                    addr: sym_addr,
                    lang,
                } = sym;
                results.push(Sym {
                    name: self.maybe_demangle(name, lang),
                    addr: sym_addr,
                    offset: addr - sym_addr,
                    dir: None,
                    file: None,
                    line: None,
                    column: None,
                    _non_exhaustive: (),
                });
            }
        }
        Ok(results)
    }

    /// Symbolize a list of addresses using the provided [`SymResolver`].
    fn symbolize_addrs(&self, addrs: &[Addr], resolver: &dyn SymResolver) -> Result<Vec<Vec<Sym>>> {
        addrs
            .iter()
            .map(|addr| self.symbolize_with_resolver(*addr, resolver))
            .collect()
    }

    fn resolve_addr_in_elf(&self, addr: Addr, path: &Path) -> Result<Vec<Sym>> {
        let backend = self.elf_cache.find(path)?;
        let resolver = ElfResolver::with_backend(path, backend)?;
        let symbols = self.symbolize_with_resolver(addr, &resolver)?;
        Ok(symbols)
    }

    /// Symbolize the given list of user space addresses in the provided
    /// process.
    fn symbolize_user_addrs(&self, addrs: &[Addr], pid: Pid) -> Result<Vec<Vec<Sym>>> {
        struct SymbolizeHandler<'sym> {
            /// The "outer" `Symbolizer` instance.
            symbolizer: &'sym Symbolizer,
            /// Symbols representing the symbolized addresses.
            all_symbols: Vec<Vec<Sym>>,
        }

        impl SymbolizeHandler<'_> {
            fn handle_apk_addr(&mut self, addr: Addr, entry: &PathMapsEntry) -> Result<()> {
                let (norm_addr, elf_path, elf_parser) = normalize_apk_addr(addr, entry)?;
                let apk_path = &entry.path.symbolic_path;
                // Create an Android-style binary-in-APK path for
                // reporting purposes.
                let apk_elf_path = create_apk_elf_path(apk_path, &elf_path)?;
                let backend = ElfBackend::Elf(Rc::new(elf_parser));

                let resolver = ElfResolver::with_backend(&apk_elf_path, backend)?;
                let symbols = self
                    .symbolizer
                    .symbolize_with_resolver(norm_addr, &resolver)?;
                let () = self.all_symbols.push(symbols);
                Ok(())
            }

            fn handle_elf_addr(&mut self, addr: Addr, entry: &PathMapsEntry) -> Result<()> {
                let path = &entry.path.maps_file;
                let norm_addr = normalize_elf_addr(addr, entry)?;
                let symbols = self
                    .symbolizer
                    .resolve_addr_in_elf(norm_addr, path)
                    .with_context(|| {
                        format!(
                            "failed to symbolize normalized address {norm_addr:#x} in ELF file {}",
                            path.display()
                        )
                    })?;
                let () = self.all_symbols.push(symbols);
                Ok(())
            }
        }

        impl normalize::Handler for SymbolizeHandler<'_> {
            #[cfg_attr(feature = "tracing", crate::log::instrument(skip_all, fields(addr = format_args!("{_addr:#x}"))))]
            fn handle_unknown_addr(&mut self, _addr: Addr) -> Result<()> {
                let () = self.all_symbols.push(Vec::new());
                Ok(())
            }

            fn handle_entry_addr(&mut self, addr: Addr, entry: &PathMapsEntry) -> Result<()> {
                let ext = entry
                    .path
                    .symbolic_path
                    .extension()
                    .unwrap_or_else(|| OsStr::new(""));
                match ext.to_str() {
                    Some("apk") | Some("zip") => self.handle_apk_addr(addr, entry),
                    _ => self.handle_elf_addr(addr, entry),
                }
            }
        }

        let entries = maps::parse(pid)?;
        let handler = SymbolizeHandler {
            symbolizer: self,
            all_symbols: Vec::with_capacity(addrs.len()),
        };

        let handler = util::with_ordered_elems(
            addrs,
            |handler: &mut SymbolizeHandler<'_>| handler.all_symbols.as_mut_slice(),
            |sorted_addrs| normalize_sorted_user_addrs_with_entries(sorted_addrs, entries, handler),
        )?;
        Ok(handler.all_symbols)
    }

    fn symbolize_kernel_addrs(&self, addrs: &[Addr], src: &Kernel) -> Result<Vec<Vec<Sym>>> {
        let Kernel {
            kallsyms,
            kernel_image,
            _non_exhaustive: (),
        } = src;

        let ksym_resolver = if let Some(kallsyms) = kallsyms {
            let ksym_resolver = self.ksym_cache.get_resolver(kallsyms)?;
            Some(ksym_resolver)
        } else {
            let kallsyms = Path::new(KALLSYMS);
            let result = self.ksym_cache.get_resolver(kallsyms);
            match result {
                Ok(resolver) => Some(resolver),
                Err(err) => {
                    log::warn!(
                        "failed to load kallsyms from {}: {err}; ignoring...",
                        kallsyms.display()
                    );
                    None
                }
            }
        };

        let elf_resolver = if let Some(image) = kernel_image {
            let backend = self.elf_cache.find(image)?;
            let elf_resolver = ElfResolver::with_backend(image, backend)?;
            Some(elf_resolver)
        } else {
            let release = uname_release()?.to_str().unwrap().to_string();
            let basename = "vmlinux-";
            let dirs = [Path::new("/boot/"), Path::new("/usr/lib/debug/boot/")];
            let kernel_image = dirs.iter().find_map(|dir| {
                let path = dir.join(format!("{basename}{release}"));
                path.exists().then_some(path)
            });

            if let Some(image) = kernel_image {
                let result = self.elf_cache.find(&image);
                match result {
                    Ok(backend) => {
                        let result = ElfResolver::with_backend(&image, backend);
                        match result {
                            Ok(resolver) => Some(resolver),
                            Err(err) => {
                                log::warn!(
                                    "failed to create ELF resolver for kernel image {}: {err}; ignoring...",
                                    image.display()
                                );
                                None
                            }
                        }
                    }
                    Err(err) => {
                        log::warn!(
                            "failed to load kernel image {}: {err}; ignoring...",
                            image.display()
                        );
                        None
                    }
                }
            } else {
                None
            }
        };

        let resolver = KernelResolver::new(ksym_resolver, elf_resolver)?;
        let symbols = self.symbolize_addrs(addrs, &resolver)?;
        Ok(symbols)
    }

    /// Symbolize a list of addresses.
    ///
    /// Symbolize a list of addresses according to the configuration
    /// provided via `src`.
    #[cfg_attr(feature = "tracing", crate::log::instrument(skip_all, fields(src = ?src, addrs = format_args!("{addrs:#x?}"))))]
    pub fn symbolize(&self, src: &Source, addrs: &[Addr]) -> Result<Vec<Vec<Sym>>> {
        match src {
            Source::Elf(Elf {
                path,
                _non_exhaustive: (),
            }) => {
                let backend = self.elf_cache.find(path)?;
                let resolver = ElfResolver::with_backend(path, backend)?;
                let symbols = self.symbolize_addrs(addrs, &resolver)?;
                Ok(symbols)
            }
            Source::Kernel(kernel) => self.symbolize_kernel_addrs(addrs, kernel),
            Source::Process(Process {
                pid,
                _non_exhaustive: (),
            }) => self.symbolize_user_addrs(addrs, *pid),
            Source::Gsym(Gsym::Data(GsymData {
                data,
                _non_exhaustive: (),
            })) => {
                let resolver = GsymResolver::with_data(data)?;
                let symbols = self.symbolize_addrs(addrs, &resolver)?;
                Ok(symbols)
            }
            Source::Gsym(Gsym::File(GsymFile {
                path,
                _non_exhaustive: (),
            })) => {
                let resolver = GsymResolver::new(path.clone())?;
                let symbols = self.symbolize_addrs(addrs, &resolver)?;
                Ok(symbols)
            }
        }
    }
}

impl Default for Symbolizer {
    fn default() -> Self {
        Self::new()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    use std::mem::transmute;

    use crate::elf::ElfParser;
    use crate::inspect::FindAddrOpts;
    use crate::inspect::SymType;
    use crate::mmap::Mmap;
    use crate::symbolize;
    use crate::symbolize::Symbolizer;
    use crate::zip;

    use test_log::test;


    /// Check that we can symbolize an address residing in a zip archive.
    #[test]
    fn symbolize_zip() {
        let test_zip = Path::new(&env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("test.zip");

        let mmap = Mmap::builder().exec().open(test_zip).unwrap();
        let archive = zip::Archive::with_mmap(mmap.clone()).unwrap();
        let so = archive
            .entries()
            .find_map(|entry| {
                let entry = entry.unwrap();
                (entry.path == Path::new("libtest-so.so")).then_some(entry)
            })
            .unwrap();

        let elf_mmap = mmap
            .constrain(so.data_offset..so.data_offset + so.data.len())
            .unwrap();

        // Look up the address of the `the_answer` function inside of the shared
        // object.
        let elf_parser = ElfParser::from_mmap(elf_mmap.clone());
        let opts = FindAddrOpts {
            sym_type: SymType::Function,
            ..Default::default()
        };
        let syms = elf_parser.find_addr("the_answer", &opts).unwrap();
        // There is only one symbol with this address in there.
        assert_eq!(syms.len(), 1);
        let sym = syms.first().unwrap();

        let the_answer_addr = unsafe { elf_mmap.as_ptr().add(sym.addr) };
        // Now just double check that everything worked out and the function
        // is actually where it was meant to be.
        let the_answer_fn =
            unsafe { transmute::<_, extern "C" fn() -> libc::c_int>(the_answer_addr) };
        let answer = the_answer_fn();
        assert_eq!(answer, 42);

        // Now symbolize the address we just looked up. It should be
        // correctly mapped to the `the_answer` function within our
        // process.
        let src = symbolize::Source::Process(symbolize::Process::new(Pid::Slf));
        let symbolizer = Symbolizer::new();
        let results = symbolizer
            .symbolize(&src, &[the_answer_addr as Addr])
            .unwrap()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1);

        let result = results.first().unwrap();
        assert_eq!(result.name, "the_answer");
        assert_eq!(result.addr, sym.addr);
    }
}
