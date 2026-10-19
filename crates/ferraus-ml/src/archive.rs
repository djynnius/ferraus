//! Unpacking multi-file models shipped as `.tar.gz` (DeepFilterNet 3: three ONNX graphs and
//! `config.ini`). Only the named members are extracted, by file name, into a fixed
//! directory, so archive paths can never escape it; extraction is atomic (temporary
//! directory, then rename) and happens once per cached archive.

use ferraus_core::{Error, Result};
use std::path::{Path, PathBuf};

/// Extract the members whose file names are `names` from `archive` into `dest` (created).
/// Returns the paths in the order of `names`. Reuses an existing complete extraction.
pub fn extract_members(archive: &Path, dest: &Path, names: &[&str]) -> Result<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = names.iter().map(|n| dest.join(n)).collect();
    if paths.iter().all(|p| p.is_file()) {
        return Ok(paths);
    }
    let parent = dest.parent().ok_or_else(|| Error::internal("extraction directory has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("extract"),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let result = (|| -> Result<()> {
        let f = std::fs::File::open(archive)?;
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
        let mut found = vec![false; names.len()];
        let entries = ar.entries().map_err(|e| Error::processing(format!("{}: {e}", archive.display())))?;
        for entry in entries {
            let mut entry = entry.map_err(|e| Error::processing(format!("{}: {e}", archive.display())))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path().map_err(|e| Error::processing(e.to_string()))?.into_owned();
            let Some(file) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if let Some(i) = names.iter().position(|n| *n == file) {
                let mut out = std::fs::File::create(tmp.join(names[i]))?;
                std::io::copy(&mut entry, &mut out)?;
                found[i] = true;
            }
        }
        if let Some(i) = found.iter().position(|f| !f) {
            return Err(Error::processing(format!("{} does not contain {}", archive.display(), names[i])));
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(dest);
    if let Err(e) = std::fs::rename(&tmp, dest) {
        // another process may have finished the same extraction first
        let _ = std::fs::remove_dir_all(&tmp);
        if !paths.iter().all(|p| p.is_file()) {
            return Err(e.into());
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::TempDir;

    fn make_archive(path: &Path, files: &[(&str, &[u8])]) {
        let f = std::fs::File::create(path).unwrap();
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn extracts_named_members_only() {
        let dir = TempDir::new("ml-archive");
        let ar = dir.path("m.tar.gz");
        make_archive(&ar, &[("tmp/export/a.onnx", b"A"), ("tmp/export/b.ini", b"B"), ("other/c", b"C")]);
        let dest = dir.path("x");
        let p = extract_members(&ar, &dest, &["a.onnx", "b.ini"]).unwrap();
        assert_eq!(std::fs::read(&p[0]).unwrap(), b"A");
        assert_eq!(std::fs::read(&p[1]).unwrap(), b"B");
        assert!(!dest.join("c").exists());
        // reuse
        let p2 = extract_members(&ar, &dest, &["a.onnx", "b.ini"]).unwrap();
        assert_eq!(p, p2);
        // missing member
        let e = extract_members(&ar, &dir.path("y"), &["a.onnx", "zzz"]).unwrap_err();
        assert!(e.message.contains("zzz"), "{}", e.message);
        assert!(!dir.path("y").exists());
    }
}
