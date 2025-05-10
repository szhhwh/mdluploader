use std::path::PathBuf;

use log::trace;
use url::Url;

#[derive(PartialEq, Debug)]
enum State {
    Normal,
    ExclamationFound,
    OpenSquareBracketFound,
    ClosingSquareBracketFound,
    OpenParenthesisFound,
    CollectingUrl,
}

/// Replace image links in Markdown content
/// # Arguments
/// - `content` - Markdown file content
/// - `path_map` - Mapping from local image paths to S3 URLs
/// # Return
/// - Markdown content with replaced image links
pub fn link_replacer(content: &str, path_map: &std::collections::HashMap<String, Url>) -> String {
    let mut result = String::new();
    let mut state = State::Normal;
    let mut alt_text = String::new();
    let mut current_url = String::new();
    let mut current_pos = 0;
    let mut is_code_block = false;
    let mut code_fence_count = 0;

    let chars: Vec<char> = content.chars().collect();
    
    while current_pos < chars.len() {
        let ch = chars[current_pos];
        
        // Process code blocks
        if ch == '`' {
            code_fence_count += 1;
            if code_fence_count == 3 {
                is_code_block = !is_code_block;
                code_fence_count = 0;
            }
        } else {
            code_fence_count = 0;
        }

        // Skip replacement if inside a code block
        if is_code_block {
            result.push(ch);
            current_pos += 1;
            continue;
        }

        match state {
            State::Normal => {
                if ch == '!' {
                    state = State::ExclamationFound;
                    result.push(ch);
                } else {
                    result.push(ch);
                }
                current_pos += 1;
            }
            State::ExclamationFound => {
                if ch == '[' {
                    state = State::OpenSquareBracketFound;
                    result.push(ch);
                } else {
                    state = State::Normal;
                    result.push(ch);
                }
                current_pos += 1;
            }
            State::OpenSquareBracketFound => {
                if ch == ']' {
                    state = State::ClosingSquareBracketFound;
                    result.push(ch);
                } else {
                    alt_text.push(ch);
                    result.push(ch);
                }
                current_pos += 1;
            }
            State::ClosingSquareBracketFound => {
                if ch == '(' {
                    state = State::OpenParenthesisFound;
                    result.push(ch);
                } else {
                    state = State::Normal;
                    result.push(ch);
                }
                current_pos += 1;
            }
            State::OpenParenthesisFound => {
                if ch == ')' {
                    // Case of empty URL
                    state = State::Normal;
                    result.push(ch);
                    current_pos += 1;
                } else {
                    current_url.push(ch);
                    state = State::CollectingUrl;
                    current_pos += 1;
                }
            }
            State::CollectingUrl => {
                if ch == ')' {
                    // URL collection complete, check if replacement is needed
                    let local_path = PathBuf::from(&current_url).file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    
                    // Try to parse absolute path
                    if let Some(s3_url) = path_map.get(&local_path) {
                        // Replace with S3 URL
                        trace!("Replacing image link: {} -> {}", current_url, s3_url);
                        result.push_str(s3_url.as_str());
                    } else {
                        // If no match is found, keep the original URL
                        trace!("No replacement found, keeping original link: {}", current_url);
                        result.push_str(&current_url);
                    }
                    
                    result.push(ch); // Add closing parenthesis
                    current_url.clear();
                    alt_text.clear();
                    state = State::Normal;
                } else {
                    current_url.push(ch);
                }
                current_pos += 1;
            }
        }
    }
    
    // Handle the state at the end
    if state == State::CollectingUrl && !current_url.is_empty() {
        // If we're collecting a URL at the end, add the collected part
        result.push_str(&current_url);
    }
    
    result
}

pub fn extract_img_urls(content: &str) -> Option<Vec<PathBuf>> {
    let mut state = State::Normal;
    let mut current_url = String::new();
    let mut urls: Vec<PathBuf> = Vec::new();
    let mut is_code_block = false;
    let mut code_fence_count = 0;

    for ch in content.chars() {
        if ch == '`' {
            code_fence_count += 1;
            if code_fence_count == 3 {
                is_code_block = !is_code_block;
                code_fence_count = 0;
                continue;
            }
        } else {
            code_fence_count = 0;
        }

        // Skip image link parsing if inside a code block
        if is_code_block {
            continue;
        }

        match state {
            State::Normal => {
                if ch == '!' {
                    state = State::ExclamationFound;
                }
            }
            State::ExclamationFound => {
                if ch == '[' {
                    state = State::OpenSquareBracketFound;
                } else {
                    state = State::Normal;
                }
            }
            State::OpenSquareBracketFound => {
                if ch == ']' {
                    state = State::ClosingSquareBracketFound;
                }
                // Ignore other characters, continue collecting content inside square brackets
            }
            State::ClosingSquareBracketFound => {
                if ch == '(' {
                    state = State::OpenParenthesisFound;
                    // Found opening parenthesis, preparing to collect URL
                } else {
                    state = State::Normal; // If not followed by (, return to initial state
                }
            }
            State::OpenParenthesisFound => {
                if ch == ')' {
                    // Case of empty URL
                    state = State::Normal;
                } else {
                    current_url.push(ch);
                    state = State::CollectingUrl;
                }
            }
            State::CollectingUrl => {
                if ch == ')' {
                    // URL collection complete, add and reset
                    urls.push(current_url.clone().into());
                    current_url.clear();
                    state = State::Normal;
                } else {
                    current_url.push(ch);
                }
            }
        }
    }
    
    // Handle potential incomplete syntax - if we're not in Normal state at the end and URL was collected
    if state == State::CollectingUrl && !current_url.is_empty() {
        current_url.clear();
    }
    
    if urls.is_empty() {
        None
    } else {
        Some(urls)
    }
}

#[cfg(test)]
mod link_extractor_test {
    use super::*;

    #[test]
    fn test_extract_single_image() {
        let content = "This is a Markdown with an image: ![Image description](path/to/image.jpg)";
        let result = extract_img_urls(content);
        assert!(result.is_some());
        let urls = result.unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], PathBuf::from("path/to/image.jpg"));
    }

    #[test]
    fn test_extract_multiple_images() {
        let content = "These are multiple images: ![Image1](image1.png) Text ![Image2](image2.jpg)";
        let result = extract_img_urls(content);
        assert!(result.is_some());
        let urls = result.unwrap();
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], PathBuf::from("image1.png"));
        assert_eq!(urls[1], PathBuf::from("image2.jpg"));
    }

    #[test]
    fn test_no_images() {
        let content = "This is a Markdown text without images";
        let result = extract_img_urls(content);
        assert!(result.is_none());
    }

    #[test]
    fn test_incomplete_markdown_syntax() {
        let content = "This is an incomplete syntax: ![Image description](path/to/image.jpg";
        let result = extract_img_urls(content);
        assert!(result.is_none());
    }

    #[test]
    fn test_with_code_blocks() {
        let content = "The image syntax in code blocks should not be extracted:\n```\n![Image](image.png)\n```\nBut this one should be extracted: ![Real image](real_image.jpg)";
        let result = extract_img_urls(content);
        assert!(result.is_some());
        let urls = result.unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], PathBuf::from("real_image.jpg"));
    }
}
