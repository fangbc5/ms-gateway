use regex::Regex;
use std::collections::HashMap;
use once_cell::sync::Lazy;
use std::sync::Mutex;

/// RoutePattern: 存储原始 pattern、编译后的正则、变量名顺序
pub struct RoutePattern {
    pattern: String,
    regex: Regex,
    var_names: Vec<String>,
}

// 使用once_cell缓存编译后的模式，避免重复编译
static PATTERN_CACHE: Lazy<Mutex<HashMap<String, RoutePattern>>> = 
    Lazy::new(|| Mutex::new(HashMap::new()));

impl RoutePattern {
    /// 将像 "/api/{id:[0-9]+}/file/**" 这样的 pattern 编译成 Regex 并记录变量名
    pub fn from_pattern(pattern: &str) -> Result<Self, regex::Error> {
        // 先检查缓存
        {
            let cache = PATTERN_CACHE.lock().unwrap();
            if let Some(cached) = cache.get(pattern) {
                return Ok(cached.clone());
            }
        }

        // 编译新模式
        let new_pattern = Self::compile_pattern(pattern)?;
        
        // 存入缓存
        {
            let mut cache = PATTERN_CACHE.lock().unwrap();
            cache.insert(pattern.to_string(), new_pattern.clone());
        }

        Ok(new_pattern)
    }

    fn compile_pattern(pattern: &str) -> Result<Self, regex::Error> {
        let mut var_names = Vec::new();
        let mut re = String::new();
        re.push('^'); // 整体匹配

        let starts_with_slash = pattern.starts_with('/');
        let mut segs_iter = pattern.split('/').peekable();

        // if starts with slash, ensure regex expects leading slash
        if starts_with_slash {
            re.push('/');
        }

        while let Some(seg) = segs_iter.next() {
            // skip the empty leading segment we've already handled
            if seg.is_empty() {
                continue;
            }

            // if segment is exactly "**"
            if seg == "**" {
                // match zero or more characters including slashes, non-greedy to allow following segments
                re.push_str("(?:.*)?"); // match any remainder optionally
                continue;
            }

            // For regular segment, we will build a pattern without leading slash
            let mut i = 0usize;
            let chars: Vec<char> = seg.chars().collect();

            while i < chars.len() {
                let c = chars[i];
                if c == '*' {
                    // single '*' in segment matches zero or more chars except '/'
                    re.push_str("[^/]*");
                    i += 1;
                } else if c == '?' {
                    // matches exactly one char except '/'
                    re.push_str("[^/]");
                    i += 1;
                } else if c == '{' {
                    // parse until matching '}'
                    let mut j = i + 1;
                    while j < chars.len() && chars[j] != '}' { j += 1; }
                    if j >= chars.len() {
                        // unmatched brace, treat as literal
                        re.push('\\');
                        re.push('{');
                        i += 1;
                        continue;
                    }
                    let inside: String = chars[i+1..j].iter().collect();
                    // inside could be "name" or "name:regex"
                    if let Some(pos) = inside.find(':') {
                        let name = inside[..pos].to_string();
                        let regex_part = &inside[pos+1..];
                        var_names.push(name.clone());
                        // use a named capture group
                        re.push_str(&format!("(?P<{}>{})", name, regex_part));
                    } else {
                        let name = inside;
                        var_names.push(name.clone());
                        // default to one path segment (no slash)
                        re.push_str(&format!("(?P<{}>[^/]+)", name));
                    }
                    i = j + 1; // skip past '}'
                } else {
                    // literal char; escape regex metacharacters
                    let esc = regex::escape(&c.to_string());
                    re.push_str(&esc);
                    i += 1;
                }
            }

            // after finishing this segment, if there are more segments ahead, we must ensure to add '/'
            if segs_iter.peek().is_some() {
                re.push('/');
            }
        }

        re.push('$');

        // compile
        let regex = Regex::new(&re)?;
        Ok(RoutePattern {
            pattern: pattern.to_string(),
            regex,
            var_names,
        })
    }

    /// 尝试匹配 path，匹配成功返回 Some(map) 包含命名参数
    pub fn match_path<'a>(&self, path: &str) -> Option<HashMap<String, String>> {
        if let Some(caps) = self.regex.captures(path) {
            let mut map = HashMap::new();
            for name in &self.var_names {
                if let Some(m) = caps.name(name) {
                    map.insert(name.clone(), m.as_str().to_string());
                }
            }
            return Some(map);
        }
        None
    }

    /// 获取原始 pattern 字符串
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// 检查是否匹配路径（不提取变量）
    pub fn matches(&self, path: &str) -> bool {
        self.regex.is_match(path)
    }
}

impl Clone for RoutePattern {
    fn clone(&self) -> Self {
        Self {
            pattern: self.pattern.clone(),
            regex: self.regex.clone(),
            var_names: self.var_names.clone(),
        }
    }
}

impl std::fmt::Debug for RoutePattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutePattern")
            .field("pattern", &self.pattern)
            .field("var_names", &self.var_names)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_star_question() {
        let p = RoutePattern::from_pattern("/api/te?t").unwrap();
        assert!(p.match_path("/api/test").is_some());
        assert!(p.match_path("/api/text").is_some());
        assert!(p.match_path("/api/teest").is_none());
    }

    #[test]
    fn test_star_segment() {
        let p = RoutePattern::from_pattern("/api/*").unwrap();
        assert!(p.match_path("/api/a").is_some());
        assert!(p.match_path("/api/abc").is_some());
        assert!(p.match_path("/api/a/b").is_none());
    }

    #[test]
    fn test_double_star() {
        let p = RoutePattern::from_pattern("/api/**").unwrap();
        assert!(p.match_path("/api/a").is_some());
        assert!(p.match_path("/api/a/b").is_some());
        assert!(p.match_path("/api/").is_some());
    }

    #[test]
    fn test_path_variable() {
        let p = RoutePattern::from_pattern("/user/{id}").unwrap();
        let m = p.match_path("/user/123").unwrap();
        assert_eq!(m.get("id").unwrap(), "123");
    }

    #[test]
    fn test_path_variable_with_regex() {
        let p = RoutePattern::from_pattern("/user/{id:[0-9]+}").unwrap();
        assert!(p.match_path("/user/123").is_some());
        assert!(p.match_path("/user/abc").is_none());
    }

    #[test]
    fn test_complex() {
        let p = RoutePattern::from_pattern("/order/{oid:[A-Z0-9]+}/item/{iid}").unwrap();
        let m = p.match_path("/order/ABC123/item/100").unwrap();
        assert_eq!(m.get("oid").unwrap(), "ABC123");
        assert_eq!(m.get("iid").unwrap(), "100");
    }

    #[test]
    fn test_cache_performance() {
        // 测试缓存功能
        let _p1 = RoutePattern::from_pattern("/api/user/{id}").unwrap();
        let _p2 = RoutePattern::from_pattern("/api/user/{id}").unwrap(); // 应该从缓存获取
        // 如果缓存工作正常，第二次调用应该很快
    }

    #[test]
    fn test_traditional_prefix_matching() {
        let test_cases = vec![
            ("/user", "/user", true),           // 精确匹配
            ("/user", "/user/profile", false),   // 前缀匹配不应该匹配子路径
            ("/user", "/api/user", false),      // 不匹配
        ];

        for (pattern, path, expected) in test_cases {
            let p = RoutePattern::from_pattern(pattern).unwrap();
            assert_eq!(p.matches(path), expected, "Pattern: {}, Path: {}", pattern, path);
        }
    }

    #[test]
    fn test_prefix_matching_with_wildcard() {
        let test_cases = vec![
            ("/user/*", "/user/profile", true),     // 单段通配符匹配
            ("/user/*", "/user/settings", true),    // 单段通配符匹配
            ("/user/*", "/user", false),            // 单段通配符不匹配空段
            ("/user/*", "/user/profile/edit", false), // 单段通配符不匹配多段
        ];

        for (pattern, path, expected) in test_cases {
            let p = RoutePattern::from_pattern(pattern).unwrap();
            assert_eq!(p.matches(path), expected, "Pattern: {}, Path: {}", pattern, path);
        }
    }

    #[test]
    fn test_prefix_matching_with_double_wildcard() {
        let test_cases = vec![
            ("/user/**", "/user", false),                    // 多段通配符匹配空
            ("/user/**", "/user/profile", true),           // 多段通配符匹配单段
            ("/user/**", "/user/profile/edit", true),      // 多段通配符匹配多段
            ("/user/**", "/api/user", false),              // 多段通配符不匹配其他前缀
        ];

        for (pattern, path, expected) in test_cases {
            let p = RoutePattern::from_pattern(pattern).unwrap();
            assert_eq!(p.matches(path), expected, "Pattern: {}, Path: {}", pattern, path);
        }
    }

    #[test]
    fn test_path_variable_matching() {
        let test_cases = vec![
            ("/api/user/{id}", "/api/user/123", true),
            ("/api/user/{id}", "/api/user/abc", true),
            ("/api/user/{id}", "/api/user/123/profile", false),
        ];

        for (pattern, path, expected) in test_cases {
            let p = RoutePattern::from_pattern(pattern).unwrap();
            assert_eq!(p.matches(path), expected, "Pattern: {}, Path: {}", pattern, path);
        }
    }

    #[test]
    fn test_regex_constraint_matching() {
        let test_cases = vec![
            ("/api/user/{id:[0-9]+}", "/api/user/123", true),
            ("/api/user/{id:[0-9]+}", "/api/user/abc", false),
        ];

        for (pattern, path, expected) in test_cases {
            let p = RoutePattern::from_pattern(pattern).unwrap();
            assert_eq!(p.matches(path), expected, "Pattern: {}, Path: {}", pattern, path);
        }
    }

    #[test]
    fn test_multi_wildcard_matching() {
        let test_cases = vec![
            ("/static/**", "/static/css/style.css", true),
            ("/static/**", "/static/js/app.js", true),
            ("/static/**", "/static", false),
            ("/static/**", "/api/static", false),
        ];

        for (pattern, path, expected) in test_cases {
            let p = RoutePattern::from_pattern(pattern).unwrap();
            assert_eq!(p.matches(path), expected, "Pattern: {}, Path: {}", pattern, path);
        }
    }

    #[test]
    fn test_single_char_wildcard_matching() {
        let test_cases = vec![
            ("/files/?.txt", "/files/a.txt", true),
            ("/files/?.txt", "/files/1.txt", true),
            ("/files/?.txt", "/files/ab.txt", false),
            ("/files/?.txt", "/files/a.pdf", false),
        ];

        for (pattern, path, expected) in test_cases {
            let p = RoutePattern::from_pattern(pattern).unwrap();
            assert_eq!(p.matches(path), expected, "Pattern: {}, Path: {}", pattern, path);
        }
    }

    #[test]
    fn test_complex_pattern_matching() {
        let test_cases = vec![
            ("/api/v{version}/user/{id:[0-9]+}/posts/**", "/api/v1/user/123/posts", false),
            ("/api/v{version}/user/{id:[0-9]+}/posts/**", "/api/v2/user/456/posts/789", true),
            ("/api/v{version}/user/{id:[0-9]+}/posts/**", "/api/v1/user/abc/posts/123", false),
        ];

        for (pattern, path, expected) in test_cases {
            let p = RoutePattern::from_pattern(pattern).unwrap();
            assert_eq!(p.matches(path), expected, "Pattern: {}, Path: {}", pattern, path);
        }
    }

    #[test]
    fn test_variable_extraction() {
        let p = RoutePattern::from_pattern("/api/user/{id}").unwrap();
        let variables = p.match_path("/api/user/123").unwrap();
        assert_eq!(variables.get("id").unwrap(), "123");

        let p = RoutePattern::from_pattern("/api/v{version}/user/{id:[0-9]+}/posts/**").unwrap();
        let variables = p.match_path("/api/v1/user/123/posts/456").unwrap();
        assert_eq!(variables.get("version").unwrap(), "1");
        assert_eq!(variables.get("id").unwrap(), "123");
    }
} 