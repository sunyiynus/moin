//! Bounded, read-only file inspection. Called by a background task, never by Render.
use anyhow::{Context, Result};
use moin::search::index::{Hit, HitKind};
use std::{
    fs::{self, File},
    io::Read,
    path::PathBuf,
    time::SystemTime,
};

const MAX_TEXT_BYTES: usize = 128 * 1024;
const MAX_TEXT_CHARS: usize = 16_000;
const MAX_DIRECTORY_ITEMS: usize = 80;
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 16_000_000;

#[derive(Clone, Debug)]
pub(super) struct FilePreview {
    pub title: String,
    pub path: PathBuf,
    pub kind: String,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub content: Content,
}

#[derive(Clone, Debug)]
pub(super) enum Content {
    Text {
        text: String,
        truncated: bool,
        chunk: Option<usize>,
    },
    Image {
        width: u32,
        height: u32,
    },
    Directory {
        entries: Vec<(String, bool)>,
        truncated: bool,
    },
    Metadata(String),
}

pub(super) fn load(hit: &Hit) -> Result<FilePreview> {
    let metadata = fs::metadata(&hit.path).context("文件不可用，可能已移动或删除")?;
    let mut preview = FilePreview {
        title: hit.title.clone(),
        path: hit.path.clone(),
        kind: if metadata.is_dir() {
            "文件夹".into()
        } else {
            hit.path
                .extension()
                .map(|ext| format!("{} 文件", ext.to_string_lossy().to_uppercase()))
                .unwrap_or("文件".into())
        },
        size: metadata.is_file().then_some(metadata.len()),
        modified: metadata.modified().ok(),
        content: Content::Metadata("此格式暂不提供正文预览，可按 Enter 用默认应用打开。".into()),
    };
    if metadata.is_dir() {
        let mut entries = Vec::new();
        let mut truncated = false;
        for (index, entry) in fs::read_dir(&hit.path)
            .context("无法读取文件夹")?
            .enumerate()
        {
            if index == MAX_DIRECTORY_ITEMS {
                truncated = true;
                break;
            }
            let entry = entry.context("无法读取目录项")?;
            entries.push((
                entry.file_name().to_string_lossy().into_owned(),
                entry.file_type().is_ok_and(|kind| kind.is_dir()),
            ));
        }
        entries.sort_by(|(a, a_dir), (b, b_dir)| {
            b_dir
                .cmp(a_dir)
                .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
        });
        preview.content = Content::Directory { entries, truncated };
    } else if metadata.is_file() {
        if hit.kind == HitKind::Text && !hit.excerpt.is_empty() {
            preview.content = Content::Text {
                text: hit.excerpt.clone(),
                truncated: false,
                chunk: hit.chunk,
            };
            return Ok(preview);
        }
        let extension = hit
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(
            extension.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
        ) {
            if metadata.len() > MAX_IMAGE_BYTES {
                preview.content =
                    Content::Metadata("图片超过 16 MiB，可按 Enter 用默认应用查看。".into());
            } else if let Ok((width, height)) = image::ImageReader::open(&hit.path)?
                .with_guessed_format()?
                .into_dimensions()
            {
                if width > 0
                    && height > 0
                    && u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS
                {
                    preview.content = Content::Image { width, height };
                } else {
                    preview.content =
                        Content::Metadata("图片尺寸过大，可按 Enter 用默认应用查看。".into());
                }
            }
        } else if !matches!(
            extension.as_str(),
            "pdf"
                | "doc"
                | "docx"
                | "xls"
                | "xlsx"
                | "ppt"
                | "pptx"
                | "zip"
                | "7z"
                | "rar"
                | "exe"
                | "dll"
                | "wasm"
                | "db"
                | "sqlite"
                | "sqlite3"
        ) {
            let mut bytes = Vec::new();
            File::open(&hit.path)
                .context("无法读取文件")?
                .take((MAX_TEXT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            let byte_truncated = bytes.len() > MAX_TEXT_BYTES;
            bytes.truncate(MAX_TEXT_BYTES);
            if let Some(text) = decode_text(&bytes, byte_truncated) {
                let char_truncated = text.chars().count() > MAX_TEXT_CHARS;
                preview.content = Content::Text {
                    text: text.chars().take(MAX_TEXT_CHARS).collect(),
                    truncated: byte_truncated || char_truncated,
                    chunk: None,
                };
            }
        }
    }
    Ok(preview)
}

fn decode_text(bytes: &[u8], truncated: bool) -> Option<String> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little_endian = bytes[0] == 0xff;
        let units = bytes[2..]
            .chunks_exact(2)
            .map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect::<Vec<_>>();
        let mut units = units.as_slice();
        if truncated
            && units
                .last()
                .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
        {
            units = &units[..units.len() - 1];
        }
        return String::from_utf16(units).ok();
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if bytes
        .iter()
        .take(4096)
        .any(|byte| *byte == 0 || (*byte < 0x20 && !matches!(*byte, b'\r' | b'\n' | b'\t' | 0x0c)))
    {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text.to_owned()),
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])
                .ok()
                .map(str::to_owned)
        }
        _ => None,
    }
}

pub(super) fn size_label(size: u64) -> String {
    if size >= 1024 * 1024 {
        format!("{:.1} MiB", size as f64 / (1024. * 1024.))
    } else if size >= 1024 {
        format!("{:.1} KiB", size as f64 / 1024.)
    } else {
        format!("{size} B")
    }
}

pub(super) fn modified_label(modified: SystemTime) -> String {
    let seconds = modified.elapsed().map_or(0, |age| age.as_secs());
    if seconds < 60 {
        "刚刚修改".into()
    } else if seconds < 3600 {
        format!("{} 分钟前修改", seconds / 60)
    } else if seconds < 86400 {
        format!("{} 小时前修改", seconds / 3600)
    } else {
        format!("{} 天前修改", seconds / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hit(path: PathBuf) -> Hit {
        Hit {
            title: "预览".into(),
            path,
            detail: String::new(),
            excerpt: String::new(),
            kind: HitKind::File,
            chunk: None,
        }
    }
    #[test]
    fn previews_unicode_and_bom_utf16_without_index_dependency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("中文.txt");
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(
            "文件正文：你好 🌏"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        fs::write(&path, bytes).unwrap();
        assert!(
            matches!(load(&hit(path)).unwrap().content, Content::Text {text,..} if text == "文件正文：你好 🌏")
        );
    }
    #[test]
    fn caps_large_text_and_does_not_show_binary_as_garbled_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        fs::write(&path, "正文".repeat(MAX_TEXT_BYTES)).unwrap();
        assert!(
            matches!(load(&hit(path)).unwrap().content, Content::Text {text,truncated:true,..} if text.chars().count() <= MAX_TEXT_CHARS)
        );
        let path = dir.path().join("binary.bin");
        fs::write(&path, [0, 0xff, 4, 5]).unwrap();
        assert!(matches!(
            load(&hit(path)).unwrap().content,
            Content::Metadata(_)
        ));
    }
    #[test]
    fn folder_previews_are_bounded_and_source_excerpts_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..90 {
            fs::write(dir.path().join(format!("{i}.txt")), "内容").unwrap();
        }
        assert!(
            matches!(load(&hit(dir.path().to_owned())).unwrap().content, Content::Directory { entries,truncated:true } if entries.len()==MAX_DIRECTORY_ITEMS)
        );
        let mut source = hit(dir.path().join("1.txt"));
        source.kind = HitKind::Text;
        source.chunk = Some(3);
        source.excerpt = "命中的原文".into();
        assert!(
            matches!(load(&source).unwrap().content, Content::Text {text,chunk:Some(3),..} if text=="命中的原文")
        );
    }
}
