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

/// 替换 Markdown 内容中的图片链接
/// # Arguments
/// - `content` - Markdown 文件内容
/// - `path_map` - 本地图片路径到 S3 URL 的映射
/// # Return
/// - 替换图片链接后的 Markdown 内容
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
        
        // 处理代码块
        if ch == '`' {
            code_fence_count += 1;
            if code_fence_count == 3 {
                is_code_block = !is_code_block;
                code_fence_count = 0;
            }
        } else {
            code_fence_count = 0;
        }

        // 如果在代码块内，跳过替换
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
                    // 空URL的情况
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
                    // URL收集完成，检查是否需要替换
                    let local_path = PathBuf::from(&current_url).file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    
                    // 尝试解析绝对路径
                    if let Some(s3_url) = path_map.get(&local_path) {
                        // 替换为S3 URL
                        trace!("替换图片链接: {} -> {}", current_url, s3_url);
                        result.push_str(s3_url.as_str());
                    } else {
                        // 如果没有匹配项，保持原有URL
                        trace!("未找到替换项，保持原有链接: {}", current_url);
                        result.push_str(&current_url);
                    }
                    
                    result.push(ch); // 添加闭括号
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
    
    // 处理结束时的状态
    if state == State::CollectingUrl && !current_url.is_empty() {
        // 如果结束时正在收集URL，添加已收集的部分
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

        // 如果在代码块内，跳过图片链接解析
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
                // 忽略其他字符，继续收集方括号内的内容
            }
            State::ClosingSquareBracketFound => {
                if ch == '(' {
                    state = State::OpenParenthesisFound;
                    // 找到开括号，准备收集URL
                } else {
                    state = State::Normal; // 如果后面不是(，回到初始状态
                }
            }
            State::OpenParenthesisFound => {
                if ch == ')' {
                    // 空URL的情况
                    state = State::Normal;
                } else {
                    current_url.push(ch);
                    state = State::CollectingUrl;
                }
            }
            State::CollectingUrl => {
                if ch == ')' {
                    // URL收集完成，添加并重置
                    urls.push(current_url.clone().into());
                    current_url.clear();
                    state = State::Normal;
                } else {
                    current_url.push(ch);
                }
            }
        }
    }
    
    // 处理可能的不完整语法情况 - 如果结束时不是Normal状态且收集了URL
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
        let content = "这是一个带有图片的 Markdown：![图片描述](path/to/image.jpg)";
        let result = extract_img_urls(content);
        assert!(result.is_some());
        let urls = result.unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], PathBuf::from("path/to/image.jpg"));
    }

    #[test]
    fn test_extract_multiple_images() {
        let content = "这是多张图片：![图片1](image1.png) 文字 ![图片2](image2.jpg)";
        let result = extract_img_urls(content);
        assert!(result.is_some());
        let urls = result.unwrap();
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], PathBuf::from("image1.png"));
        assert_eq!(urls[1], PathBuf::from("image2.jpg"));
    }

    #[test]
    fn test_no_images() {
        let content = "这是没有图片的 Markdown 文本";
        let result = extract_img_urls(content);
        assert!(result.is_none());
    }

    #[test]
    fn test_incomplete_markdown_syntax() {
        let content = "这是不完整的语法: ![图片描述](path/to/image.jpg";
        let result = extract_img_urls(content);
        assert!(result.is_none());
    }

    #[test]
    fn test_with_code_blocks() {
        let content = "代码块中的图片语法不应该被提取：\n```\n![图片](image.png)\n```\n但这个应该被提取：![真实图片](real_image.jpg)";
        let result = extract_img_urls(content);
        assert!(result.is_some());
        let urls = result.unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], PathBuf::from("real_image.jpg"));
    }
}
