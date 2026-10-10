//! Conversion between `file://` URIs and filesystem paths.

use std::path::{Path, PathBuf};

pub fn to_path(uri: &str) -> Option<PathBuf> {
    let path = if let Some(rest) = uri.strip_prefix("file://") {
        // Drop an optional authority ("file://localhost/path").
        match rest.find('/') {
            Some(0) => rest,
            Some(index) => &rest[index..],
            None => return None,
        }
    } else {
        // RFC 8089 "file:/path": no authority at all.
        let rest = uri.strip_prefix("file:")?;
        if !rest.starts_with('/') {
            return None;
        }
        rest
    };
    let decoded = percent_decode(path);
    // "/C:/dir" on Windows.
    let bytes = decoded.as_bytes();
    if cfg!(windows)
        && bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[2] == b':'
    {
        return Some(PathBuf::from(&decoded[1..]));
    }
    Some(PathBuf::from(decoded))
}

pub fn from_path(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    let mut uri = String::from("file://");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// The decoded file name of a URI, or its raw last segment when it is not a file path.
pub fn file_name(uri: &str) -> String {
    to_path(uri)
        .and_then(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| {
            uri.rsplit('/')
                .next()
                .unwrap_or(uri)
                .to_string()
        })
}

/// A key that identifies the same file regardless of how its URI is encoded.
pub fn key(uri: &str) -> String {
    match to_path(uri) {
        Some(path) => canonical(&path)
            .to_string_lossy()
            .into_owned(),
        None => uri.to_string(),
    }
}

/// `path` without its `.` and `..` segments (symbolic links resolved, as the
/// operating system does for `..`), for walking up to a project folder. A
/// path without such segments is returned as it is.
pub fn resolve_dots(path: &Path) -> PathBuf {
    use std::path::Component;
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        let resolved = canonical(path);
        // The nonexistent tail may still hold segments: remove them lexically.
        let mut out = PathBuf::new();
        for component in resolved.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        return out;
    }
    path.to_path_buf()
}

/// `path` with symbolic links resolved. For a file that does not exist yet,
/// its nearest existing ancestor is resolved instead, so that the key stays
/// the same once the file is saved.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    for ancestor in path.ancestors().skip(1) {
        if let (Ok(base), Ok(rest)) =
            (std::fs::canonicalize(ancestor), path.strip_prefix(ancestor))
        {
            return base.join(rest);
        }
    }
    path.to_path_buf()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(value) =
                hex.and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                decoded.push(value);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn keys_stay_the_same_when_a_new_file_is_saved() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-uri-{}", std::process::id()));
        let link = std::env::temp_dir()
            .join(format!("gsql-uri-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let uri = from_path(&link.join("new.gsql"));
        let before = key(&uri);
        std::fs::write(dir.join("new.gsql"), "").unwrap();
        let after = key(&uri);
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn round_trips_paths() {
        let path = Path::new("/tmp/my dir/q#1.gsql");
        let uri = from_path(path);
        assert_eq!(uri, "file:///tmp/my%20dir/q%231.gsql");
        assert_eq!(to_path(&uri).unwrap(), path);
    }

    #[test]
    fn accepts_every_spelling_of_a_file_uri() {
        let expected = Path::new("/a/b é.gsql");
        for uri in [
            "file:///a/b%20%C3%A9.gsql",
            "file:/a/b%20%C3%A9.gsql",
            "file://localhost/a/b%20%C3%A9.gsql",
            "file://LOCALHOST/a/b%20%C3%A9.gsql",
        ] {
            assert_eq!(to_path(uri).as_deref(), Some(expected), "{uri}");
        }
        assert_eq!(key("file:/a/b.gsql"), key("file:///a/b.gsql"));
        assert_eq!(key("file://localhost/a/b.gsql"), key("file:///a/b.gsql"));
        assert_eq!(to_path("file:a/b.gsql"), None);
        assert_eq!(to_path("file:"), None);
        assert_eq!(to_path("file://host"), None);
    }

    #[test]
    fn resolves_dot_segments() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-dots-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        let base = std::fs::canonicalize(&dir).unwrap();
        assert_eq!(
            resolve_dots(&dir.join("a/../b/q.gsql")),
            base.join("b/q.gsql")
        );
        assert_eq!(
            resolve_dots(&dir.join("a/./../b/missing/../q.gsql")),
            base.join("b/q.gsql")
        );
        assert_eq!(resolve_dots(&dir.join("a/q.gsql")), dir.join("a/q.gsql"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn decodes_client_uris() {
        assert_eq!(
            to_path("file:///a/b%C3%A9.gsql").unwrap(),
            Path::new("/a/bé.gsql")
        );
        assert_eq!(
            to_path("file://localhost/a/b.gsql").unwrap(),
            Path::new("/a/b.gsql")
        );
        assert_eq!(to_path("untitled:Untitled-1"), None);
    }
}
