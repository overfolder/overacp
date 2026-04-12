//! Resolve `file:///` URLs in multimodal content blocks to inline
//! `data:` URLs so that LLM APIs can consume them directly.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::Value;
use std::fs;
use std::path::Path;
use tracing::warn;

use super::{Content, ContentBlock, Message, TypedBlock};

/// Resolve `file:///` URLs in all messages in place.
pub fn resolve_file_urls(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        resolve_file_urls_in_message(msg);
    }
}

/// Resolve `file:///` URLs in a single message in place.
pub fn resolve_file_urls_in_message(msg: &mut Message) {
    if let Some(Content::Blocks(blocks)) = &mut msg.content {
        for typed in blocks.iter_mut() {
            if let TypedBlock::Known(block) = typed {
                resolve_content_block(block);
            }
        }
    }
}

fn resolve_content_block(block: &mut ContentBlock) {
    let ContentBlock::ImageUrl { image_url } = block else {
        return;
    };

    let Some(url_str) = image_url.get("url").and_then(Value::as_str) else {
        return;
    };

    if !url_str.starts_with("file:///") {
        return;
    }

    let path_str = &url_str[7..]; // strip "file://"

    match resolve_file_url(path_str) {
        Ok(data_url) => {
            image_url["url"] = Value::String(data_url);
        }
        Err(err) => {
            warn!(path = path_str, %err, "failed to resolve file URL");
            *block = ContentBlock::Text {
                text: format!("[Image not available: {path_str}: {err}]"),
            };
        }
    }
}

fn resolve_file_url(path_str: &str) -> Result<String, String> {
    let path = Path::new(path_str);

    let mime = mime_from_extension(path)
        .ok_or_else(|| format!("unsupported file extension: {path_str}"))?;

    let bytes = fs::read(path).map_err(|e| e.to_string())?;

    let encoded = STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

fn mime_from_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "pdf" => Some("application/pdf"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Role;
    use fs::{remove_file, rename, write};
    use serde_json::json;

    fn image_url_block(url: &str) -> ContentBlock {
        ContentBlock::ImageUrl {
            image_url: json!({ "url": url }),
        }
    }

    #[test]
    fn passthrough_https_url() {
        let mut block = image_url_block("https://example.com/img.png");
        resolve_content_block(&mut block);
        match &block {
            ContentBlock::ImageUrl { image_url } => {
                assert_eq!(image_url["url"], "https://example.com/img.png");
            }
            _ => panic!("expected ImageUrl"),
        }
    }

    #[test]
    fn passthrough_data_url() {
        let url = "data:image/png;base64,abc";
        let mut block = image_url_block(url);
        resolve_content_block(&mut block);
        match &block {
            ContentBlock::ImageUrl { image_url } => {
                assert_eq!(image_url["url"], url);
            }
            _ => panic!("expected ImageUrl"),
        }
    }

    #[test]
    fn resolve_file_url_png() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let png_path = path.with_extension("png");
        rename(&path, &png_path).unwrap();
        let _guard = scopeguard(&png_path);

        let data = b"\x89PNG\r\n\x1a\nfake";
        write(&png_path, data).unwrap();
        drop(tmp);

        let url = format!("file://{}", png_path.display());
        let mut block = image_url_block(&url);
        resolve_content_block(&mut block);

        match &block {
            ContentBlock::ImageUrl { image_url } => {
                let resolved = image_url["url"].as_str().unwrap();
                assert!(resolved.starts_with("data:image/png;base64,"));
                let encoded_part = resolved.strip_prefix("data:image/png;base64,").unwrap();
                let decoded = STANDARD.decode(encoded_part).unwrap();
                assert_eq!(decoded, data);
            }
            _ => panic!("expected ImageUrl, got {:?}", block),
        }
    }

    #[test]
    fn resolve_missing_file() {
        let mut block = image_url_block("file:///nonexistent/path/image.png");
        resolve_content_block(&mut block);
        match &block {
            ContentBlock::Text { text } => {
                assert!(text.contains("Image not available"));
                assert!(text.contains("/nonexistent/path/image.png"));
            }
            _ => panic!("expected Text error block"),
        }
    }

    #[test]
    fn resolve_unknown_extension() {
        let tmp = tempfile::NamedTempFile::with_suffix(".bmp").unwrap();
        let url = format!("file://{}", tmp.path().display());
        let mut block = image_url_block(&url);
        resolve_content_block(&mut block);
        match &block {
            ContentBlock::Text { text } => {
                assert!(text.contains("unsupported file extension"));
            }
            _ => panic!("expected Text error block"),
        }
    }

    #[test]
    fn mime_detection() {
        assert_eq!(mime_from_extension(Path::new("x.png")), Some("image/png"));
        assert_eq!(mime_from_extension(Path::new("x.jpg")), Some("image/jpeg"));
        assert_eq!(mime_from_extension(Path::new("x.jpeg")), Some("image/jpeg"));
        assert_eq!(mime_from_extension(Path::new("x.gif")), Some("image/gif"));
        assert_eq!(mime_from_extension(Path::new("x.webp")), Some("image/webp"));
        assert_eq!(
            mime_from_extension(Path::new("x.pdf")),
            Some("application/pdf")
        );
        assert_eq!(mime_from_extension(Path::new("x.bmp")), None);
        assert_eq!(mime_from_extension(Path::new("noext")), None);
    }

    #[test]
    fn resolve_message_mixed_blocks() {
        let tmp = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        write(tmp.path(), b"fakepng").unwrap();
        let file_url = format!("file://{}", tmp.path().display());

        let mut msg = Message {
            role: Role::User,
            content: Some(Content::Blocks(vec![
                TypedBlock::Known(ContentBlock::Text {
                    text: "describe these".into(),
                }),
                TypedBlock::Known(image_url_block(&file_url)),
                TypedBlock::Known(image_url_block("https://example.com/other.png")),
            ])),
            tool_calls: None,
            tool_call_id: None,
        };

        resolve_file_urls_in_message(&mut msg);

        let blocks = match &msg.content {
            Some(Content::Blocks(b)) => b,
            _ => panic!("expected Blocks"),
        };

        // Text block unchanged
        assert!(blocks[0].as_text() == Some("describe these"));
        // file:// resolved to data:
        match &blocks[1] {
            TypedBlock::Known(ContentBlock::ImageUrl { image_url }) => {
                assert!(image_url["url"]
                    .as_str()
                    .unwrap()
                    .starts_with("data:image/png;base64,"));
            }
            _ => panic!("expected resolved ImageUrl"),
        }
        // https:// unchanged
        match &blocks[2] {
            TypedBlock::Known(ContentBlock::ImageUrl { image_url }) => {
                assert_eq!(image_url["url"], "https://example.com/other.png");
            }
            _ => panic!("expected unchanged ImageUrl"),
        }
    }

    #[test]
    fn resolve_text_message_noop() {
        let mut msg = Message {
            role: Role::User,
            content: Some(Content::Text("hello".into())),
            tool_calls: None,
            tool_call_id: None,
        };
        resolve_file_urls_in_message(&mut msg);
        assert_eq!(msg.content.as_ref().unwrap().as_text(), Some("hello"));
    }

    /// RAII guard that deletes a path on drop (for renamed tempfiles).
    fn scopeguard(path: &Path) -> impl Drop + '_ {
        struct Guard<'a>(&'a Path);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                let _ = remove_file(self.0);
            }
        }
        Guard(path)
    }
}
