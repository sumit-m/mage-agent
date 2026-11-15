//! Makes captured terminal output readable in a chat code block.

const MAX_CHARS: usize = 3000;

pub fn clean(raw: &str) -> String {
    let without_escapes = strip_escapes(raw);
    let lines: Vec<&str> = without_escapes
        .split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.rsplit('\r').next().unwrap_or(line).trim_end()
        })
        .collect();
    let text = lines.join("\n").trim_matches('\n').to_string();
    truncate_head(&text).replace("```", "``\u{200b}`")
}

fn truncate_head(text: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_CHARS {
        return text.to_string();
    }
    let tail: String = text.chars().skip(count - MAX_CHARS).collect();
    format!("…(earlier output truncated)\n{tail}")
}

fn strip_escapes(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c == '\u{8}' {
                out.pop();
            } else if !c.is_control() || c == '\n' || c == '\r' || c == '\t' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colors_and_progress_redraws() {
        let raw = "\u{1b}[32mok\u{1b}[0m\r\nprogress 10%\rprogress 50%\rprogress 100%\n\u{1b}]0;title\u{7}done\n";
        assert_eq!(clean(raw), "ok\nprogress 100%\ndone");
    }

    #[test]
    fn neutralizes_code_fences() {
        assert_eq!(clean("a ``` b"), "a ``\u{200b}` b");
    }

    #[test]
    fn keeps_the_tail_of_long_output() {
        let raw = "x".repeat(MAX_CHARS + 10);
        let cleaned = clean(&raw);
        assert!(cleaned.starts_with("…(earlier output truncated)\n"));
        assert!(cleaned.ends_with(&"x".repeat(MAX_CHARS)));
    }
}
