use std::path::PathBuf;

#[derive(PartialEq, Debug)]
enum State {
    Normal,
    ExclamationFound,
    OpenSquareBracketFound,
    ClosingSquareBracketFound,
    OpenParenthesisFound,
    CollectingUrl,
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
        // Markdown语法不完整，不应添加该URL
    }
    
    if urls.is_empty() {
        None
    } else {
        Some(urls)
    }
}

#[cfg(test)]
mod tests {
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
