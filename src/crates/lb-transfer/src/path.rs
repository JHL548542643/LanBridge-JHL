use crate::error::{Error, Result};
use std::path::{Component, Path, PathBuf};

/// Validate a relative transfer path: `/` separators, no `..`, no absolute, no NUL.
pub fn sanitize_relative_path(raw: &str) -> Result<String> {
    if raw.is_empty() || raw.len() > 1024 {
        return Err(Error::BadPath("empty or too long"));
    }
    if raw.contains('\0') || raw.contains('\\') {
        return Err(Error::BadPath("illegal character"));
    }
    if raw.starts_with('/') {
        return Err(Error::BadPath("absolute path"));
    }
    let path = Path::new(raw);
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str().ok_or(Error::BadPath("non-utf8 segment"))?;
                if s == "." || s.is_empty() {
                    continue;
                }
                parts.push(s);
            }
            Component::CurDir => {}
            _ => return Err(Error::BadPath("parent or prefix component")),
        }
    }
    if parts.is_empty() {
        return Err(Error::BadPath("empty after sanitize"));
    }
    Ok(parts.join("/"))
}

fn strip_verbatim(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn is_under_root(root: &Path, candidate: &Path) -> bool {
    let root = strip_verbatim(root);
    let candidate = strip_verbatim(candidate);
    candidate.starts_with(&root)
}

/// Canonicalize `dest_root`; reject if it is (or resolves through) a symlink.
pub fn ensure_canonical_root(dest_root: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dest_root)?;
    let meta = std::fs::symlink_metadata(dest_root)?;
    if meta.file_type().is_symlink() {
        return Err(Error::BadPath("dest_root is symlink"));
    }
    let canon = std::fs::canonicalize(dest_root)?;
    Ok(canon)
}

/// Join `relative` under `root` without following symlink components.
fn join_under_root(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut cur = root.to_path_buf();
    let parts: Vec<&str> = relative.split('/').filter(|p| !p.is_empty()).collect();
    for (i, part) in parts.iter().enumerate() {
        cur.push(part);
        let is_last = i + 1 == parts.len();
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(Error::BadPath("symlink in destination path"));
                }
                if !is_last && !meta.is_dir() {
                    return Err(Error::BadPath("path component not a directory"));
                }
            }
            Err(_) if !is_last => {
                std::fs::create_dir(&cur)?;
            }
            Err(_) => {}
        }
    }
    let parent = cur.parent().unwrap_or(root);
    let parent_check = if parent.exists() {
        std::fs::canonicalize(parent)?
    } else {
        parent.to_path_buf()
    };
    if !is_under_root(root, &parent_check) {
        return Err(Error::BadPath("escaped dest_root"));
    }
    Ok(cur)
}

/// Place `relative` under `dest_root`. If the file exists, rename with ` (n)` before extension.
pub fn resolve_dest_path(dest_root: &Path, relative: &str, overwrite: bool) -> Result<PathBuf> {
    let relative = sanitize_relative_path(relative)?;
    let root = ensure_canonical_root(dest_root)?;
    let full = join_under_root(&root, &relative)?;
    if overwrite || !full.exists() {
        return Ok(full);
    }
    let parent = full.parent().unwrap_or(&root);
    let stem = full.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = full.extension().and_then(|s| s.to_str());
    for n in 1..10_000 {
        let name = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = parent.join(name);
        if let Ok(meta) = std::fs::symlink_metadata(&candidate) {
            if meta.file_type().is_symlink() {
                continue;
            }
        }
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(Error::BadPath("could not find free name"))
}

/// Resolve receive path + resume offset; same symlink / containment rules as [`resolve_dest_path`].
pub fn pick_dest_for_recv(
    dest_root: &Path,
    relative: &str,
    file_size: u64,
    overwrite: bool,
) -> Result<(PathBuf, u64)> {
    let relative = sanitize_relative_path(relative)?;
    let root = ensure_canonical_root(dest_root)?;
    let full = join_under_root(&root, &relative)?;
    if !full.exists() {
        return Ok((full, 0));
    }
    let meta = std::fs::symlink_metadata(&full)?;
    if meta.file_type().is_symlink() {
        return Err(Error::BadPath("symlink in destination path"));
    }
    let existing = std::fs::metadata(&full)?.len();
    if existing < file_size {
        return Ok((full, existing));
    }
    if overwrite {
        return Ok((full, 0));
    }
    let parent = full.parent().unwrap_or(&root);
    let stem = full.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = full.extension().and_then(|s| s.to_str());
    for n in 1..10_000 {
        let name = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = parent.join(name);
        if let Ok(meta) = std::fs::symlink_metadata(&candidate) {
            if meta.file_type().is_symlink() {
                continue;
            }
        }
        if !candidate.exists() {
            return Ok((candidate, 0));
        }
    }
    Err(Error::BadPath("could not find free name"))
}

/// Regular files under `root`, relative `/` paths, symlinks skipped (not followed).
pub fn list_files(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    walk_dir(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn walk_dir(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for ent in std::fs::read_dir(dir)? {
        let ent = ent?;
        let ft = ent.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let path = ent.path();
        if ft.is_dir() {
            walk_dir(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| Error::BadPath("prefix"))?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            let rel = sanitize_relative_path(&rel)?;
            out.push((rel, path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_dotdot() {
        assert!(sanitize_relative_path("../x").is_err());
        assert!(sanitize_relative_path("a/../b").is_err());
    }

    #[test]
    fn accepts_nested() {
        assert_eq!(sanitize_relative_path("a/b.txt").unwrap(), "a/b.txt");
    }

    #[test]
    fn rejects_symlink_component() {
        let dir = std::env::temp_dir().join(format!(
            "lb-path-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("inbox")).unwrap();
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let link = dir.join("inbox").join("trap");
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(&outside, &link).is_err() {
                let _ = fs::remove_dir_all(&dir);
                return; // needs privilege on some Windows hosts
            }
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link).unwrap();
        }
        let err = resolve_dest_path(&dir, "inbox/trap/evil.bin", false);
        assert!(err.is_err(), "expected symlink reject, got {err:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
