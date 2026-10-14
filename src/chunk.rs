use std::path::Path;

pub struct Chunk {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

pub fn supported(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "c"
            | "cpp"
            | "cc"
            | "h"
            | "hpp"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "cs"
            | "rb"
            | "php"
            | "sh"
            | "bash"
            | "zsh"
            | "sql"
            | "html"
            | "css"
            | "scss"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "xml"
            | "md"
            | "mdx"
            | "txt"
            | "rst"
            | "adoc"
            | "csv"
            | "tsv"
            | "log"
            | "srt"
            | "ipynb"
            | "tex"
            | "ini"
            | "conf"
            | "cfg"
            | "dockerfile"
            | "makefile"
    ) || matches!(
        path.file_name().and_then(|x| x.to_str()).unwrap_or(""),
        "Dockerfile" | "Makefile" | "LICENSE" | "AGENTS.md"
    )
}

pub fn media_kind(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "bmp" => Some("image"),
        "wav" | "mp3" | "flac" | "ogg" | "m4a" => Some("audio"),
        "mp4" | "mov" | "mkv" | "webm" => Some("video"),
        _ => None,
    }
}

pub fn split(text: &str, target_chars: usize, overlap_lines: usize) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut pos = 0;
    while pos < lines.len() {
        let mut end = pos;
        let mut len = 0;
        while end < lines.len() {
            let n = lines[end].chars().count() + 1;
            if end > pos && len + n > target_chars {
                break;
            }
            len += n;
            end += 1;
        }
        let excerpt = lines[pos..end].join("\n");
        if !excerpt.trim().is_empty() {
            // Oversized single lines (minified code, JSON, long paragraphs) must
            // also be bounded; split at Unicode boundaries and keep the line ID.
            if end == pos + 1 && excerpt.chars().count() > target_chars.max(1) {
                let chars: Vec<char> = excerpt.chars().collect();
                for part in chars.chunks(target_chars.max(1)) {
                    result.push(Chunk {
                        text: part.iter().collect(),
                        start: pos + 1,
                        end,
                    });
                }
            } else {
                result.push(Chunk {
                    text: excerpt,
                    start: pos + 1,
                    end,
                });
            }
        }
        if end == lines.len() {
            break;
        }
        // Guarantee forward progress even if overlap is larger than the chunk.
        pos = end.saturating_sub(overlap_lines).max(pos + 1);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_unicode_line_is_bounded_and_lossless() {
        let line = "가나다라마".repeat(300);
        let chunks = split(&line, 1100, 2);
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .iter()
                .all(|c| c.text.chars().count() <= 1100 && c.start == 1 && c.end == 1)
        );
        assert_eq!(
            chunks.iter().map(|c| c.text.as_str()).collect::<String>(),
            line
        );
    }

    #[test]
    fn chunks_cover_last_line() {
        let chunks = split("one\ntwo\nthree\nfour\nfive", 9, 1);
        assert_eq!(chunks.first().unwrap().start, 1);
        assert_eq!(chunks.last().unwrap().end, 5);
        assert!(chunks.iter().any(|x| x.text.contains("three")));
    }
}
