//! Turns a chat message into mage CLI arguments plus the Buzz-only `--repo` flag.

#[derive(Debug, PartialEq)]
pub struct Request {
    pub args: Vec<String>,
    pub repo: Option<String>,
}

impl Request {
    pub fn subcommand(&self) -> &str {
        self.args.first().map_or("", String::as_str)
    }

    pub fn produces_images(&self) -> bool {
        matches!(self.subcommand(), "generate" | "gen")
    }

    pub fn has_flag(&self, flag: &str) -> bool {
        let with_value = format!("{flag}=");
        self.args
            .iter()
            .any(|a| a == flag || a.starts_with(&with_value))
    }
}

pub const USAGE: &str = "Usage: `@mage <mage command and flags> [--repo [<repo>:]<path>]`\n\
e.g. `@mage gen a red fox --aspect landscape --repo assets/` — every mage flag except `--output` is passed through.";

pub fn parse_request(
    content: &str,
    attachment_urls: &[String],
    display_name: Option<&str>,
) -> Result<Request, String> {
    let text = without_attachment_lines(content, attachment_urls);
    let text = unwrap_links(&strip_formatting(&text));
    let text = normalize_typography(&text);
    let text = strip_display_name(text.trim(), display_name);
    let mut tokens = split_words(text);
    let leading_mentions = tokens
        .iter()
        .take_while(|t| t.starts_with('@') || t.starts_with("nostr:"))
        .count();
    tokens.drain(..leading_mentions);

    let mut args = Vec::new();
    let mut repo = None;
    let mut iter = tokens.into_iter();
    while let Some(token) = iter.next() {
        if token == "--output" || token.starts_with("--output=") {
            return Err("`--output` is set by mage-agent; leave it out.".into());
        }
        if token == "--repo" {
            repo = Some(iter.next().ok_or("`--repo` needs a path")?);
        } else if let Some(value) = token.strip_prefix("--repo=") {
            repo = Some(value.to_string());
        } else {
            args.push(token);
        }
    }

    let request = Request {
        args: without_edit(args),
        repo,
    };
    if request.args.is_empty() {
        return Err(USAGE.into());
    }
    if request.repo.is_some() && !request.produces_images() {
        return Err(format!(
            "`--repo` only works with `generate` or `gen`, not `{}`.",
            request.subcommand()
        ));
    }
    Ok(request)
}

/// mage 0.7 folded `edit --src` into `generate --image`; older messages
/// still read the old way.
fn without_edit(mut args: Vec<String>) -> Vec<String> {
    if args.first().map(String::as_str) != Some("edit") {
        return args;
    }
    args[0] = "generate".into();
    for arg in args.iter_mut().skip(1) {
        if arg == "--src" {
            *arg = "--image".into();
        } else if let Some(value) = arg.strip_prefix("--src=") {
            *arg = format!("--image={value}");
        }
    }
    args
}

pub(crate) fn without_attachment_lines(content: &str, attachment_urls: &[String]) -> String {
    content
        .lines()
        .filter(|line| {
            !attachment_urls
                .iter()
                .any(|url| !url.is_empty() && line.contains(url.as_str()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bold, italic, strikethrough and code formatting reach the content as `**`,
/// `*`, `~~` and backticks (pasted text often carries bold); none of them
/// belong in mage arguments.
pub fn strip_formatting(text: &str) -> String {
    text.replace("~~", "").replace(['*', '`'], "")
}

/// Buzz's composer sends a typed URL as `<url>`, or `[text](url)` when the
/// link text differs; mage needs the bare URL.
fn unwrap_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(['<', '[']) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match link_at(tail) {
            Some((url, consumed)) => {
                out.push_str(&url);
                rest = &tail[consumed..];
            }
            None => {
                out.push_str(&tail[..1]);
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn link_at(tail: &str) -> Option<(String, usize)> {
    let is_url = |s: &str| s.contains("://") && !s.contains(char::is_whitespace);
    if let Some(body) = tail.strip_prefix('<') {
        let end = body.find('>')?;
        let url = &body[..end];
        return is_url(url).then(|| (url.to_string(), end + 2));
    }
    let label_end = tail.find("](")?;
    if tail[1..label_end].contains(['[', ']', '\n']) {
        return None;
    }
    let href_start = label_end + 2;
    let mut url = String::new();
    let mut chars = tail[href_start..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => url.push(chars.next()?.1),
            ')' => return is_url(&url).then(|| (url, href_start + i + 1)),
            _ => url.push(c),
        }
    }
    None
}

fn normalize_typography(text: &str) -> String {
    text.replace(['\u{201C}', '\u{201D}'], "\"")
        .replace(['\u{2018}', '\u{2019}'], "'")
        .split_whitespace()
        .map(|word| match word.strip_prefix(['\u{2014}', '\u{2013}']) {
            Some(rest) => format!("--{rest}"),
            None => word.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn strip_display_name<'a>(text: &'a str, display_name: Option<&str>) -> &'a str {
    let Some(name) = display_name.map(str::trim).filter(|n| !n.is_empty()) else {
        return text;
    };
    let mention = format!("@{name}");
    match text.get(..mention.len()) {
        Some(head) if head.eq_ignore_ascii_case(&mention) => text[mention.len()..].trim_start(),
        _ => text,
    }
}

/// Splits on whitespace, grouping `"…"` and `'…'`. Backslashes are ordinary
/// characters so Windows paths survive unquoted. A lone apostrophe
/// (`a cat's hat`) leaves a quote unbalanced; that falls back to plain words.
fn split_words(text: &str) -> Vec<String> {
    split_quoted(text).unwrap_or_else(|| text.split_whitespace().map(String::from).collect())
}

fn split_quoted(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            None => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(content: &str) -> Result<Request, String> {
        parse_request(content, &[], Some("mage"))
    }

    #[test]
    fn passes_args_through_and_extracts_repo() {
        let r = parse("@mage gen \"a red fox\" --aspect landscape --repo art:assets/").unwrap();
        assert_eq!(r.args, ["gen", "a red fox", "--aspect", "landscape"]);
        assert_eq!(r.repo.as_deref(), Some("art:assets/"));
        assert!(r.produces_images());
    }

    #[test]
    fn repo_equals_form() {
        let r = parse("@mage gen fox --repo=img/fox.jpg").unwrap();
        assert_eq!(r.repo.as_deref(), Some("img/fox.jpg"));
    }

    #[test]
    fn rejects_output() {
        assert!(parse("@mage gen fox --output x").is_err());
        assert!(parse("@mage gen fox --output=x").is_err());
    }

    #[test]
    fn repo_requires_image_command() {
        assert!(parse("@mage jobs --repo x").is_err());
        assert!(parse("@mage gen fox --repo").is_err());
    }

    #[test]
    fn non_image_commands_pass_through() {
        assert_eq!(parse("@mage cancel").unwrap().args, ["cancel"]);
        assert_eq!(parse("@mage jobs --json").unwrap().args, ["jobs", "--json"]);
    }

    #[test]
    fn empty_is_usage() {
        assert_eq!(parse("@mage").unwrap_err(), USAGE);
    }

    #[test]
    fn multi_word_display_name_and_npub_mentions() {
        let r = parse_request("@Mage Bot gen fox", &[], Some("Mage Bot")).unwrap();
        assert_eq!(r.args, ["gen", "fox"]);
        let r = parse_request("nostr:npub1abc gen fox", &[], None).unwrap();
        assert_eq!(r.args, ["gen", "fox"]);
    }

    #[test]
    fn apostrophes_and_smart_punctuation() {
        let r = parse("@mage gen a cat\u{2019}s hat").unwrap();
        assert_eq!(r.args, ["gen", "a", "cat's", "hat"]);
        let r = parse("@mage gen \u{201C}a fox\u{201D} \u{2014}aspect square").unwrap();
        assert_eq!(r.args, ["gen", "a fox", "--aspect", "square"]);
    }

    #[test]
    fn windows_paths_keep_backslashes() {
        let r = parse(r"@mage edit --src C:\pics\cat.png add a hat").unwrap();
        assert_eq!(
            r.args,
            ["generate", "--image", r"C:\pics\cat.png", "add", "a", "hat"]
        );
        let r = parse(r#"@mage edit --src "C:\My Pics\cat.png" add a hat"#).unwrap();
        assert_eq!(r.args[2], r"C:\My Pics\cat.png");
        let r = parse(r"@mage edit --src='D:\a b\c.png' x").unwrap();
        assert_eq!(r.args[1], r"--image=D:\a b\c.png");
    }

    #[test]
    fn composer_links_become_bare_urls() {
        let r = parse("@mage edit --src <https://cdn.example/a.png?x=1&y=2> add a hat").unwrap();
        assert_eq!(r.args[2], "https://cdn.example/a.png?x=1&y=2");
        let r = parse("@mage edit --src [a.png](https://cdn.example/a\\(1\\).png) hat").unwrap();
        assert_eq!(r.args[2], "https://cdn.example/a(1).png");
    }

    #[test]
    fn non_link_brackets_are_kept() {
        let r = parse("@mage gen a <red> fox [style] (soft)").unwrap();
        assert_eq!(r.args, ["gen", "a", "<red>", "fox", "[style]", "(soft)"]);
        let r = parse("@mage gen [a](b) fox").unwrap();
        assert_eq!(r.args, ["gen", "[a](b)", "fox"]);
    }

    #[test]
    fn pasted_bold_and_code_formatting_is_dropped() {
        let r = parse(
            "@mage edit --src bee-garden:**assets/f4e3.jpg \"change the fox to blue\" --repo assets/**",
        )
        .unwrap();
        assert_eq!(
            r.args,
            [
                "generate",
                "--image",
                "bee-garden:assets/f4e3.jpg",
                "change the fox to blue"
            ]
        );
        assert_eq!(r.repo.as_deref(), Some("assets/"));
        let r = parse("@mage gen `a fox` ~~old~~ *soft* my_file").unwrap();
        assert_eq!(r.args, ["gen", "a", "fox", "old", "soft", "my_file"]);
    }

    #[test]
    fn empty_quotes_are_an_argument() {
        assert_eq!(
            parse("@mage gen --prompt \"\"").unwrap().args,
            ["gen", "--prompt", ""]
        );
    }

    #[test]
    fn attachment_markdown_is_dropped() {
        let url = "https://relay.example/media/abc.png".to_string();
        let content = format!("@mage edit make it blue\n![photo.png]({url})");
        let r = parse_request(&content, &[url], Some("mage")).unwrap();
        assert_eq!(r.args, ["generate", "make", "it", "blue"]);
    }
}
