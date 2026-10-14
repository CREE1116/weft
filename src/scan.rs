//! Filesystem discovery and exclusions shared by desktop and folder indexing.
use std::path::{Path, PathBuf};

/// Filesystem metadata used only as a fast unchanged-file hint. Force an
/// occasional full-hash scan to verify sources on unreliable filesystems.
pub fn file_stamp(meta: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{}:{}:{}:{}:{}:{}:{}",
            meta.len(),
            meta.dev(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec()
        )
    }
    #[cfg(not(unix))]
    {
        format!(
            "{}:{:?}:{:?}",
            meta.len(),
            meta.modified().ok(),
            meta.created().ok()
        )
    }
}

pub fn computer_root() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(format!(
            "{}\\",
            std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into())
        ))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/")
    }
}

/// Shared across the first scan and every watch event. Empty selects all types.
pub fn selected_type(path: &Path, selected: &[String]) -> bool {
    if selected.is_empty() {
        return true;
    }
    let category = match crate::document::format(path) {
        Some("pdf") => "pdf",
        Some("docx" | "odt") => "word",
        Some("pptx" | "odp") => "slides",
        Some("xlsx" | "ods") => "sheets",
        Some("epub" | "hwpx") => "ebooks",
        _ if crate::chunk::supported(path) => "text",
        _ => crate::chunk::media_kind(path).unwrap_or("other"),
    };
    selected.iter().any(|value| value == category)
}

pub fn excluded(path: &Path, computer: bool, extra: &[PathBuf]) -> bool {
    if extra.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    if computer {
        for root in [
            "/System",
            "/Applications",
            "/Library",
            "/usr",
            "/bin",
            "/sbin",
            "/dev",
            "/proc",
            "/sys",
            "/run",
            "/private",
            "/tmp",
            "/var",
            "/etc",
            "/lib",
            "/lib64",
            "/opt/homebrew",
            "/opt/local",
            "/snap",
        ] {
            if path.starts_with(root) {
                return true;
            }
        }
        if cfg!(windows)
            && path.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some(
                        "Windows"
                            | "Program Files"
                            | "Program Files (x86)"
                            | "ProgramData"
                            | "$Recycle.Bin"
                    )
                )
            })
        {
            return true;
        }
        // Per-user application data, caches and credential stores are not documents.
        if path.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some(
                    "Library"
                        | "AppData"
                        | ".cache"
                        | ".local"
                        | ".codex"
                        | ".agents"
                        | ".Trash"
                        | ".gemini"
                        | ".claude"
                        | ".vscode"
                        | ".vscode-server"
                        | ".mozilla"
                        | ".gradle"
                        | ".m2"
                        | ".nuget"
                        | ".nvm"
                        | ".pyenv"
                        | ".rbenv"
                        | ".conda"
                )
            )
        }) {
            return true;
        }
    }
    if path.components().any(|c| {
        let name = c.as_os_str().to_str().unwrap_or("");
        if (name.starts_with(".venv-")
            || name.starts_with(".venv_")
            || name.starts_with("venv-")
            || name.starts_with("venv_"))
            && Path::new(name).extension().is_none()
        {
            return true;
        }
        matches!(
            Some(name),
            Some(
                ".git"
                    | ".weft"
                    | ".myvec"
                    | "target"
                    | "node_modules"
                    | ".venv"
                    | "venv"
                    | "__pycache__"
                    | "site-packages"
                    | ".ssh"
                    | ".gnupg"
                    | ".aws"
                    | ".azure"
                    | ".kube"
                    | ".npm"
                    | ".cargo"
                    | ".rustup"
            )
        )
    }) {
        return true;
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.starts_with("~$")
        || matches!(
            name.as_str(),
            ".ds_store" | ".localized" | "thumbs.db" | "desktop.ini"
        )
        || name == ".env"
        || name.starts_with(".env.")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || matches!(name.as_str(), "credentials" | "credentials.json")
        || matches!(
            path.extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "pem" | "key" | "p12" | "pfx" | "kdbx"
        )
}

pub fn filename_text(path: &Path) -> String {
    format!(
        "File name: {}\nFolder: {}\nFile type: {}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        path.parent().unwrap_or(Path::new("")).display(),
        path.extension().unwrap_or_default().to_string_lossy()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn os_metadata_exclusions_match_only_the_filename() {
        for name in [
            ".DS_Store",
            ".localized",
            "Thumbs.db",
            "desktop.ini",
            "THUMBS.DB",
        ] {
            for computer in [false, true] {
                assert!(excluded(&Path::new("/docs").join(name), computer, &[]));
                assert!(!excluded(
                    &Path::new("/docs").join(name).join("notes.txt"),
                    computer,
                    &[]
                ));
            }
            assert!(!excluded(
                &Path::new("/docs").join(format!("{name}.txt")),
                false,
                &[]
            ));
        }
    }
    #[test]
    fn selected_file_types_are_case_insensitive_and_shared_with_watch() {
        let selected = vec!["pdf".into(), "word".into(), "image".into()];
        for name in ["lecture.PDF", "report.docx", "report.ODT", "picture.png"] {
            assert!(selected_type(Path::new(name), &selected));
        }
        for name in [
            "slides.pptx",
            "table.xlsx",
            "book.epub",
            "main.rs",
            "sound.mp3",
            "movie.mp4",
            "archive.zip",
        ] {
            assert!(!selected_type(Path::new(name), &selected));
        }
        assert!(selected_type(Path::new("anything.bin"), &[]));
        assert!(selected_type(Path::new("main.rs"), &["text".into()]));
        assert!(selected_type(Path::new("archive.zip"), &["other".into()]));
    }
    #[test]
    fn exclusions_prune_sensitive_and_generated_paths() {
        for p in [
            "/Users/me/.ssh/id_rsa",
            "/Users/me/project/node_modules/a.txt",
            "/Users/me/project/.venv-cutout/lib/python3.12/site-packages/a.py",
            "/Users/me/project/venv_test/bin/python",
            "/Users/me/project/python/lib/site-packages/a.py",
            "/Users/me/project/.env.local",
            "/Users/me/private.pem",
            "/System/Volumes/Data/a.txt",
            "/Users/me/Library/Caches/a.txt",
            "/opt/homebrew/lib/generated.txt",
            "/Users/me/.gemini/antigravity-browser-profile/cache.txt",
        ] {
            assert!(excluded(Path::new(p), true, &[]), "{p}");
        }
        assert!(!excluded(
            Path::new("/Users/me/Documents/venv-notes.md"),
            true,
            &[]
        ));
        assert!(!excluded(
            Path::new("/Users/me/Documents/plan.txt"),
            true,
            &[]
        ));
        assert!(!excluded(
            Path::new("/Volumes/Data/photos/trip.jpg"),
            true,
            &[]
        ));
        assert!(excluded(
            Path::new("/Users/me/Downloads/a.txt"),
            true,
            &["/Users/me/Downloads".into()]
        ));
        assert!(excluded(Path::new("/docs/~$report.docx"), false, &[]));
        assert!(!excluded(Path::new("/docs/report.docx"), false, &[]));
    }
}
