use std::ops::Range;

use log::trace;
use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};

/// Percent-decodes a markdown link destination (e.g. `my%20image.png`).
pub fn percent_decode(dest: &str) -> String {
    percent_decode_str(dest).decode_utf8_lossy().to_string()
}

/// Returns true when a link destination points at a remote resource instead
/// of a local file.
///
/// Absolute URLs with a multi-character scheme (`http:`, `https:`, `data:`,
/// ...) and protocol-relative URLs (`//cdn.example.com/x.png`) are remote.
/// Single-character schemes are Windows drive letters (`C:\\...`) and stay
/// local; bare relative paths stay local.
fn is_remote_url(dest: &str) -> bool {
    if dest.starts_with("//") {
        return true;
    }
    match url::Url::parse(dest) {
        // A parsed URL always has a scheme; treat single-letter schemes as
        // Windows drive paths rather than remote URLs.
        Ok(url) => url.scheme().len() > 1,
        Err(_) => false,
    }
}

/// Extracts the destinations of all local image links in a markdown document.
///
/// Uses pulldown-cmark, so code blocks, inline code spans, HTML blocks,
/// reference-style images (`![alt][ref]`) and titles
/// (`![alt](img.png "title")`) are all handled per CommonMark. Remote
/// destinations (http/https/protocol-relative) are skipped.
///
/// # Arguments
/// - `content` - Markdown file content.
///
/// # Return
/// - Raw (still percent-encoded) destination strings in document order.
pub fn extract_image_links(content: &str) -> Vec<String> {
    let parser = Parser::new_ext(content, Options::empty());
    let mut urls = Vec::new();

    for (event, _range) in parser.into_offset_iter() {
        if let Event::Start(Tag::Image { dest_url, .. }) = event {
            if !is_remote_url(&dest_url) && !dest_url.is_empty() {
                trace!("Image link detected: {}", dest_url);
                urls.push(dest_url.into_string());
            }
        }
    }

    urls
}

/// Locates the byte range of the inline link destination inside the raw span
/// of an inline image (`![alt](dest "title")`).
///
/// The scan follows CommonMark rules: an angle-bracket destination ends at
/// the closing `>`; a plain destination ends at the first whitespace (title
/// follows) or at the `)` that closes the construct, while unescaped
/// balanced parentheses stay part of the destination.
fn locate_inline_url_span(span: &str) -> Option<Range<usize>> {
    let open = span.find("](")?;
    let start = open + 2;
    let rest = span.get(start..)?;
    let bytes = rest.as_bytes();

    if bytes.first() == Some(&b'<') {
        // Replace the whole bracketed destination including the angle
        // brackets; inserted URLs do not need them.
        let mut i = 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'>' => return Some(start..start + i + 1),
                _ => i += 1,
            }
        }
        return None;
    }

    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            i += 2;
            continue;
        }
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            if depth == 0 {
                return Some(start..start + i);
            }
            depth -= 1;
        } else if b == b' ' || b == b'\t' || b == b'\n' {
            // Destination ended; an optional title follows until the final ')'.
            return Some(start..start + i);
        }
        i += 1;
    }
    None
}

/// Replaces local image destinations in a markdown document.
///
/// `resolve` receives each raw (still percent-encoded) local image
/// destination and returns the replacement URL string, or `None` to keep the
/// original. Everything outside the replaced destinations is preserved
/// byte-for-byte. Only inline images (`![alt](dest)`) are rewritten;
/// reference-style images keep their text because the URL lives in a
/// separate definition.
///
/// # Arguments
/// - `content` - Markdown file content.
/// - `resolve` - Closure mapping a raw destination to an optional replacement.
///
/// # Return
/// - The rewritten markdown content.
pub fn replace_image_links<F>(content: &str, resolve: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    // Collect (span, replacement) pairs first so replacements do not
    // invalidate subsequent byte offsets.
    let mut replacements: Vec<(Range<usize>, String)> = Vec::new();

    for (event, range) in Parser::new_ext(content, Options::empty()).into_offset_iter() {
        if let Event::Start(Tag::Image {
            link_type: LinkType::Inline,
            dest_url,
            ..
        }) = event
        {
            if is_remote_url(&dest_url) {
                continue;
            }
            let Some(replacement) = resolve(&dest_url) else {
                continue;
            };
            let span = &content[range.start..range.end];
            let Some(url_range) = locate_inline_url_span(span) else {
                trace!(
                    "Could not locate destination of image {}; keeping original",
                    dest_url
                );
                continue;
            };
            trace!("Replacing image link: {} -> {}", dest_url, replacement);
            replacements.push((
                range.start + url_range.start..range.start + url_range.end,
                replacement,
            ));
        }
    }

    if replacements.is_empty() {
        return content.to_string();
    }

    let mut result = String::with_capacity(content.len() + 128);
    let mut cursor = 0;
    for (range, replacement) in replacements {
        result.push_str(&content[cursor..range.start]);
        result.push_str(&replacement);
        cursor = range.end;
    }
    result.push_str(&content[cursor..]);
    result
}

#[cfg(test)]
mod link_extractor_test {
    use super::*;

    #[test]
    fn test_extract_single_image() {
        let content = "This is a Markdown with an image: ![Image description](path/to/image.jpg)";
        let urls = extract_image_links(content);
        assert_eq!(urls, vec!["path/to/image.jpg".to_string()]);
    }

    #[test]
    fn test_extract_multiple_images() {
        let content = "These are multiple images: ![Image1](image1.png) Text ![Image2](image2.jpg)";
        let urls = extract_image_links(content);
        assert_eq!(
            urls,
            vec!["image1.png".to_string(), "image2.jpg".to_string()]
        );
    }

    #[test]
    fn test_no_images() {
        let content = "This is a Markdown text without images";
        assert!(extract_image_links(content).is_empty());
    }

    #[test]
    fn test_incomplete_markdown_syntax() {
        let content = "This is an incomplete syntax: ![Image description](path/to/image.jpg";
        assert!(extract_image_links(content).is_empty());
    }

    #[test]
    fn test_with_code_blocks() {
        let content = "The image syntax in code blocks should not be extracted:\n```\n![Image](image.png)\n```\nBut this one should be extracted: ![Real image](real_image.jpg)";
        let urls = extract_image_links(content);
        assert_eq!(urls, vec!["real_image.jpg".to_string()]);
    }

    #[test]
    fn test_indented_code_block_is_skipped() {
        let content = "Text:\n\n    ![Image](code.png)\n\nThen ![Real](real.jpg)";
        let urls = extract_image_links(content);
        assert_eq!(urls, vec!["real.jpg".to_string()]);
    }

    #[test]
    fn test_inline_code_span_is_skipped() {
        let content = "Run `![Image](inline.png)` literally, but use ![Real](real.jpg).";
        let urls = extract_image_links(content);
        assert_eq!(urls, vec!["real.jpg".to_string()]);
    }

    #[test]
    fn test_image_with_title() {
        let content = "![Alt](img.png \"The Title\")";
        let urls = extract_image_links(content);
        assert_eq!(urls, vec!["img.png".to_string()]);
    }

    #[test]
    fn test_reference_style_image_is_extracted() {
        let content = "![alt][ref]\n\n[ref]: refs/image.png";
        let urls = extract_image_links(content);
        assert_eq!(urls, vec!["refs/image.png".to_string()]);
    }

    #[test]
    fn test_remote_urls_are_skipped() {
        let content =
            "![a](https://example.com/a.png) ![b](//cdn.example.com/b.png) ![c](local.png)";
        let urls = extract_image_links(content);
        assert_eq!(urls, vec!["local.png".to_string()]);
    }

    #[test]
    fn test_image_nested_in_link_is_extracted() {
        let content = "[![alt](thumb.png)](https://example.com/big.png)";
        let urls = extract_image_links(content);
        assert_eq!(urls, vec!["thumb.png".to_string()]);
    }

    #[test]
    fn test_destination_with_spaces_is_not_a_link() {
        // Plain destinations cannot contain spaces; CommonMark treats this
        // as literal text, so nothing is extracted.
        let content = "![a](my img.png)";
        assert!(extract_image_links(content).is_empty());
    }

    #[test]
    fn test_percent_encoded_destination_is_returned_raw() {
        let content = "![a](my%20image.png)";
        let urls = extract_image_links(content);
        assert_eq!(urls, vec!["my%20image.png".to_string()]);
    }

    #[test]
    fn test_percent_decode_helper() {
        assert_eq!(percent_decode("my%20image.png"), "my image.png");
        assert_eq!(percent_decode("plain.png"), "plain.png");
    }
}

#[cfg(test)]
mod link_replacer_test {
    use super::*;

    const REPLACEMENT: &str = "https://cdn.example.com/img.png";

    fn resolver(dest: &str) -> Option<String> {
        if dest == "img.png" {
            Some(REPLACEMENT.to_string())
        } else {
            None
        }
    }

    #[test]
    fn replaces_matching_inline_image() {
        let content = "Before ![alt](img.png) after";
        let out = replace_image_links(content, resolver);
        assert_eq!(out, format!("Before ![alt]({REPLACEMENT}) after"));
    }

    #[test]
    fn keeps_title_when_replacing() {
        let content = "![alt](img.png \"Title\")";
        let out = replace_image_links(content, resolver);
        assert_eq!(out, format!("![alt]({REPLACEMENT} \"Title\")"));
    }

    #[test]
    fn replaces_only_matching_images() {
        let content = "![keep](other.png) ![swap](img.png)";
        let out = replace_image_links(content, resolver);
        assert_eq!(out, format!("![keep](other.png) ![swap]({REPLACEMENT})"));
    }

    #[test]
    fn keeps_content_byte_identical_when_nothing_matches() {
        let content = "![a](x.png)\n\n```\n![b](img.png)\n```\n[link](img.png)";
        let out = replace_image_links(content, |dest| {
            // Even if the resolver wants to replace, code blocks must win.
            if dest == "x.png" {
                None
            } else {
                unreachable!("resolver must not see non-image or code content");
            }
        });
        assert_eq!(out, content);
    }

    #[test]
    fn does_not_touch_code_blocks_or_links() {
        let content = "Text with `![a](img.png)` inline and:\n\n```\n![b](img.png)\n```\n\nand a plain [link](img.png).";
        let out = replace_image_links(content, resolver);
        assert_eq!(out, content);
    }

    #[test]
    fn handles_multiple_replacements_in_one_document() {
        let content = "![1](img.png) middle ![2](img.png)";
        let out = replace_image_links(content, resolver);
        assert_eq!(
            out,
            format!("![1]({REPLACEMENT}) middle ![2]({REPLACEMENT})")
        );
    }

    #[test]
    fn replaces_angle_bracket_destination() {
        let content = "![alt](<my image.png>)";
        let out = replace_image_links(content, |dest| {
            if dest == "my image.png" {
                Some(REPLACEMENT.to_string())
            } else {
                None
            }
        });
        assert_eq!(out, format!("![alt]({REPLACEMENT})"));
    }

    #[test]
    fn skips_remote_images() {
        let content = "![a](https://example.com/img.png)";
        let out = replace_image_links(content, |_| Some(REPLACEMENT.to_string()));
        assert_eq!(out, content);
    }

    #[test]
    fn keeps_reference_style_images_untouched() {
        // The destination lives in a separate definition; the inline text is
        // not rewritten.
        let content = "![alt][ref]\n\n[ref]: img.png";
        let out = replace_image_links(content, resolver);
        assert_eq!(out, content);
    }

    #[test]
    fn preserves_surrounding_multibyte_text() {
        let content = "中文图片 ![图片](img.png) 结束";
        let out = replace_image_links(content, resolver);
        assert_eq!(out, format!("中文图片 ![图片]({REPLACEMENT}) 结束"));
    }
}
