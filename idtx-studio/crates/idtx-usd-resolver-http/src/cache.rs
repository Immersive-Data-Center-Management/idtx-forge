//! Cache names and all filesystem operations stay behind a directory capability.

use std::io;
use std::path::Path;

use cap_std::fs::Dir;
use percent_encoding::percent_decode_str;
use sha2::{Digest, Sha256};

pub(super) struct Cache {
    pub(super) directory: Dir,
}

impl Cache {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(path)?;
        Ok(Self { directory: Dir::open_ambient_dir(path, cap_std::ambient_authority())? })
    }

    pub(super) fn read(&self, leaf: &str) -> io::Result<std::fs::File> {
        let file = self.directory.open(leaf)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cache entry is not a regular file"));
        }
        Ok(file.into_std())
    }
}

/// Never join URL text as a path. Validate before WHATWG URL parsing can turn
/// backslashes into slashes or discard encoded dot segments. Queries distinguish
/// cache entries; fragments do not participate in an HTTP request.
pub(super) fn cache_leaf(input: &str) -> io::Result<String> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "unsafe asset URL path");
    let clean = input.split(['?', '#']).next().ok_or_else(invalid)?;
    if clean.contains('\\') || clean.chars().any(char::is_control) { return Err(invalid()); }
    let authority = clean.split_once("://").ok_or_else(invalid)?.1;
    let path = authority.find('/').map_or("", |start| &authority[start..]);
    for segment in path.split('/') {
        let decoded = percent_decode_str(segment).decode_utf8().map_err(|_| invalid())?;
        if decoded.chars().any(|c| c.is_control() || "/\\:<>\"|*?".contains(c)) {
            return Err(invalid());
        }
    }
    let last = path.rsplit('/').next().unwrap_or_default();
    let last = percent_decode_str(last).decode_utf8().map_err(|_| invalid())?;
    if last == "." || last == ".." || last.ends_with(['.', ' ']) { return Err(invalid()); }

    let mut url = url::Url::parse(input).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() { return Err(invalid()); }
    url.set_fragment(None);
    let extension = url.path().rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    let extension = match extension.as_str() {
        "usd" | "usda" | "usdc" | "usdz" | "png" | "jpg" | "jpeg" | "webp" |
        "exr" | "hdr" | "tga" | "tif" | "tiff" | "bmp" | "dds" | "ktx" | "ktx2" |
        "tx" | "vdb" | "abc" | "obj" | "mtl" | "gltf" | "glb" => extension.as_str(),
        _ => "bin",
    };
    Ok(format!("{:x}.{extension}", Sha256::digest(url.as_str().as_bytes())))
}

/// Transfer a capability-opened file to the CRT used by the C++ resolver. Both
/// sides use the dynamic CRT on Windows. ArFilesystemAsset closes it on drop.
pub(super) fn into_c_file(file: std::fs::File) -> io::Result<*mut libc::FILE> {
    #[cfg(unix)]
    let fd = {
        use std::os::fd::IntoRawFd;
        file.into_raw_fd()
    };
    #[cfg(windows)]
    let fd = {
        use std::os::windows::io::{AsRawHandle, IntoRawHandle};
        // SAFETY: the handle is live; ownership transfers only on success.
        let fd = unsafe { libc::open_osfhandle(file.as_raw_handle() as libc::intptr_t, libc::O_RDONLY | libc::O_BINARY) };
        if fd == -1 { return Err(io::Error::last_os_error()); }
        let _ = file.into_raw_handle();
        fd
    };
    // SAFETY: fd is owned and readable, and the mode is a terminated C string.
    let stream = unsafe { libc::fdopen(fd, c"rb".as_ptr()) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopen failed, so fd still belongs to us.
        unsafe { libc::close(fd); }
        return Err(error);
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_path_syntax_is_rejected_on_every_platform() {
        for path in [
            r"C:\Users\reviewer\proof.usda", "C:/Users/proof.usda", r"..\proof.usda", r"C:proof.usda",
            r"\proof.usda", r"\\server\share\proof.usda", r"\\?\C:\proof.usda",
            r"\\.\C:\proof.usda", "proof.usda:stream", ".", "..", "%2e", "%2E%2e",
            "%2fproof.usda", "%5Cproof.usda", "C%3Aproof.usda", "%00proof.usda",
            "proof.usda.", "proof.usda%20", "x%2fy.usda", "x%5cy.usda", "C%3A%5Cproof.usda",
        ] {
            assert!(cache_leaf(&format!("https://assets.example.com/{path}")).is_err(), "{path}");
        }
    }

    #[test]
    fn canonical_urls_produce_only_fixed_safe_leaves() {
        for (path, extension) in [
            ("scene.usda", "usda"), ("/server/share/scene.usdc", "usdc"),
            ("a/../scene.USDZ", "usdz"), ("a/./scene.usd", "usd"),
            ("scene.usda?download=C:\\outside#fragment", "usda"),
            ("scene.usda#../outside.png", "usda"), ("scene%2eusda", "bin"),
            ("CON.usda", "usda"), ("unknown.exe", "bin"), ("", "bin"),
            ("scene%255cusda.usda", "usda"),
        ] {
            let leaf = cache_leaf(&format!("https://assets.example.com/{path}")).unwrap();
            assert_eq!(leaf.len(), 65 + extension.len(), "{path}");
            assert!(leaf[..64].bytes().all(|b| b.is_ascii_hexdigit()), "{leaf}");
            assert_eq!(&leaf[65..], extension);
            assert_eq!(Path::new(&leaf).components().count(), 1);
        }
        let base = cache_leaf("https://assets.example.com/scene.usda").unwrap();
        for equivalent in [
            "https://ASSETS.example.com:443/scene.usda#fragment",
            "https://assets.example.com/a/../scene.usda",
            "https://assets.example.com/a/%2e%2e/scene.usda",
        ] { assert_eq!(cache_leaf(equivalent).unwrap(), base); }
        assert_ne!(cache_leaf("https://assets.example.com/scene.usda?v=2").unwrap(), base);
    }

    #[test]
    fn retained_root_survives_ancestor_replacement() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache");
        let cache = Cache::open(&path).unwrap();
        // Some Windows filesystems pin an open directory against renaming,
        // preventing the race. Where renaming succeeds, replace the ambient path.
        match std::fs::rename(&path, temp.path().join("original")) {
            Ok(()) => {}
            Err(error) if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("rename cache: {error}"),
        }
        std::fs::create_dir(&path).unwrap();
        let leaf = cache_leaf("https://assets.example.com/scene.usda").unwrap();
        std::fs::write(path.join(&leaf), "sentinel").unwrap();
        let mut file = cap_tempfile::TempFile::new(&cache.directory).unwrap();
        file.write_all(b"asset").unwrap();
        file.replace(&leaf).unwrap();
        assert_eq!(std::fs::read(path.join(&leaf)).unwrap(), b"sentinel");
        assert_eq!(cache.directory.read(&leaf).unwrap(), b"asset");
    }
}
