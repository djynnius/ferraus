//! Run-time loading of optional system codec libraries (libmp3lame, libopus). Nothing is
//! linked at build time, so ferraus builds with no C toolchain and the LGPL LAME library is
//! only ever used dynamically.

use libloading::Library;
use std::path::PathBuf;

/// Candidate file names/paths for a library, most specific first. `env` overrides all.
fn candidates(env: &str, base: &str, soname_major: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os(env).filter(|p| !p.is_empty()) {
        v.push(PathBuf::from(p));
    }
    if cfg!(target_os = "macos") {
        let names = [
            format!("lib{base}.{soname_major}.dylib"),
            format!("lib{base}.dylib"),
        ];
        for n in &names {
            v.push(PathBuf::from(n));
        }
        for dir in ["/opt/homebrew/lib", "/usr/local/lib", "/opt/local/lib"] {
            for n in &names {
                v.push(PathBuf::from(dir).join(n));
            }
        }
    } else if cfg!(windows) {
        for n in [
            format!("{base}.dll"),
            format!("lib{base}.dll"),
            format!("lib{base}-{soname_major}.dll"),
        ] {
            v.push(PathBuf::from(n));
        }
    } else {
        for n in [
            format!("lib{base}.so.{soname_major}"),
            format!("lib{base}.so"),
        ] {
            v.push(PathBuf::from(n));
        }
    }
    v
}

/// Try to open a library from its candidates; returns the library and the name that worked.
pub fn open(env: &str, base: &str, soname_major: &str) -> Option<(Library, String)> {
    if std::env::var_os(format!("{env}_DISABLE")).is_some_and(|v| v == "1") {
        return None;
    }
    for c in candidates(env, base, soname_major) {
        // SAFETY: loading a well-known system codec library; its initializers are benign.
        if let Ok(lib) = unsafe { Library::new(&c) } {
            return Some((lib, c.display().to_string()));
        }
    }
    None
}

/// Copy a symbol out of a library as a plain function pointer (the library must outlive it;
/// libraries opened here are leaked into statics so this holds).
///
/// # Safety
/// `T` must be the correct function pointer type for the symbol.
pub unsafe fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Option<T> {
    lib.get::<T>(name).ok().map(|s| *s)
}
