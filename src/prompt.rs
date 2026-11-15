//! Extracts the triggering events from a buzz-acp `session/prompt` text.
//!
//! The text is framed for language models, not as a stable API, so this parser
//! only relies on the `format_event_block` layout and the `<context>` reply
//! instruction, and ignores everything else.

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: String,
    pub channel: String,
    pub content: String,
    pub tags: Vec<Vec<String>>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Turn {
    pub events: Vec<Event>,
    pub reply_to: Option<String>,
}

/// Sections whose event blocks were already delivered or are only background.
const SKIPPED_SECTIONS: [&str; 5] = [
    "what-you-were-working-on",
    "previous-request-interrupted-before-completion",
    "thread-context",
    "conversation-context",
    "channel-context",
];

pub fn parse_turn(text: &str) -> Turn {
    let reply_to = section_body(text, "context").and_then(reply_anchor);
    let mut remaining = text.to_string();
    for tag in SKIPPED_SECTIONS {
        remaining = remove_sections(&remaining, tag);
    }
    Turn {
        events: event_blocks(&remaining),
        reply_to,
    }
}

pub fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn is_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// `Channel:` renders as `<uuid>` or `<name> (#<uuid>)`.
pub fn channel_uuid(display: &str) -> Option<String> {
    let display = display.trim();
    if is_uuid(display) {
        return Some(display.to_string());
    }
    let start = display.rfind("(#")? + 2;
    let candidate = display.get(start..start + 36)?;
    is_uuid(candidate).then(|| candidate.to_string())
}

fn section_body<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>\n");
    let close = format!("\n</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

fn reply_anchor(context: &str) -> Option<String> {
    context
        .match_indices("--reply-to ")
        .find_map(|(at, needle)| {
            let id = context.get(at + needle.len()..at + needle.len() + 64)?;
            is_hex64(id).then(|| id.to_string())
        })
}

fn remove_sections(text: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = find_open_tag(rest, &open) {
        out.push_str(&rest[..start]);
        match rest[start..].find(&close) {
            Some(end) => rest = &rest[start + end + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn find_open_tag(text: &str, open: &str) -> Option<usize> {
    text.match_indices(open)
        .map(|(i, _)| i)
        .find(|&i| matches!(text.as_bytes().get(i + open.len()), Some(b'>') | Some(b' ')))
}

fn event_blocks(text: &str) -> Vec<Event> {
    let mut events = Vec::new();
    let mut search_from = 0;
    while let Some(found) = text[search_from..].find("Event ID: ") {
        let start = search_from + found;
        search_from = start + 1;
        let at_line_start = start == 0 || text.as_bytes()[start - 1] == b'\n';
        if !at_line_start {
            continue;
        }
        if let Some((event, consumed)) = parse_event_block(&text[start..]) {
            search_from = start + consumed;
            events.push(event);
        }
    }
    events
}

fn parse_event_block(block: &str) -> Option<(Event, usize)> {
    let mut lines = block.splitn(6, '\n');
    let id = lines.next()?.strip_prefix("Event ID: ")?.trim().to_string();
    let channel = channel_uuid(lines.next()?.strip_prefix("Channel: ")?)?;
    lines.next()?.strip_prefix("Kind: ")?;
    lines.next()?.strip_prefix("From: ")?;
    lines.next()?.strip_prefix("Time: ")?;
    let tail = lines.next()?.strip_prefix("Content: ")?;
    let tail_offset = block.len() - tail.len();

    let mut cursor = 0;
    while let Some(found) = tail[cursor..].find("\nTags: [") {
        let tags_start = cursor + found + "\nTags: ".len();
        let line_end = tail[tags_start..]
            .find('\n')
            .map_or(tail.len(), |n| tags_start + n);
        if let Ok(tags) = serde_json::from_str::<Vec<Vec<String>>>(&tail[tags_start..line_end]) {
            let event = Event {
                id,
                channel,
                content: tail[..cursor + found].to_string(),
                tags,
            };
            return Some((event, tail_offset + line_end));
        }
        cursor += found + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const CH: &str = "0f8b2c9e-1d2a-4b3c-9d4e-5f6a7b8c9d0e";

    fn block(id: &str, content: &str) -> String {
        format!(
            "Event ID: {id}\nChannel: art (#{CH})\nKind: 9\nFrom: Alex (npub: npub1x, hex: ab)\n\
             Time: 2026-11-15T10:00:00+00:00\nContent: {content}\n\
             Tags: [[\"h\",\"{CH}\"],[\"p\",\"cc\"]]\nParsed: mentions=[mage]"
        )
    }

    #[test]
    fn single_event_with_multiline_content() {
        let text = format!(
            "<context>\nScope: channel\nChannel: art (#{CH})\nIMPORTANT: call `buzz messages send` WITHOUT `--reply-to`, so\n</context>\n\n\
             <buzz-event type=\"mention\">\n{}\n</buzz-event>",
            block(ID_A, "@mage gen a cat\nsecond line")
        );
        let turn = parse_turn(&text);
        assert_eq!(turn.reply_to, None);
        assert_eq!(turn.events.len(), 1);
        let event = &turn.events[0];
        assert_eq!(event.id, ID_A);
        assert_eq!(event.channel, CH);
        assert_eq!(event.content, "@mage gen a cat\nsecond line");
        assert_eq!(event.tags[0], vec!["h".to_string(), CH.to_string()]);
    }

    #[test]
    fn thread_reply_anchor_comes_from_context() {
        let text = format!(
            "<context>\nScope: thread\nThread root: {ID_B}\nIMPORTANT: use `--reply-to {ID_B}` on `buzz messages send`, which\n</context>\n\
             <buzz-event type=\"mention\">\n{}\n</buzz-event>",
            block(ID_A, "@mage gen x")
        );
        assert_eq!(parse_turn(&text).reply_to.as_deref(), Some(ID_B));
    }

    #[test]
    fn merged_turn_skips_prior_events() {
        let text = format!(
            "<what-you-were-working-on>\n--- Event 1 (mention) ---\n{}\n</what-you-were-working-on>\n\
             <new-message-arrived-while-you-were-working>\n--- Event 1 (mention) ---\n{}\n</new-message-arrived-while-you-were-working>",
            block(ID_A, "@mage gen old"),
            block(ID_B, "@mage gen new"),
        );
        let turn = parse_turn(&text);
        assert_eq!(turn.events.len(), 1);
        assert_eq!(turn.events[0].id, ID_B);
    }

    #[test]
    fn batched_events_all_parse() {
        let text = format!(
            "<buzz-events count=\"2\">\n--- Event 1 (mention) ---\n{}\n\n--- Event 2 (mention) ---\n{}\n</buzz-events>",
            block(ID_A, "@mage gen one"),
            block(ID_B, "@mage gen two"),
        );
        let ids: Vec<_> = parse_turn(&text).events.into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![ID_A.to_string(), ID_B.to_string()]);
    }

    #[test]
    fn content_containing_fake_tags_line_is_kept() {
        let text = block(ID_A, "@mage gen a\nTags: [not json\nmore");
        let turn = parse_turn(&text);
        assert_eq!(turn.events[0].content, "@mage gen a\nTags: [not json\nmore");
    }

    #[test]
    fn context_sections_are_ignored() {
        let text = format!(
            "<channel-context included=\"1\" total=\"1\" truncated=\"false\">\n{}\n</channel-context>",
            block(ID_A, "@mage gen ghost")
        );
        assert!(parse_turn(&text).events.is_empty());
    }

    #[test]
    fn heartbeat_without_events_is_empty() {
        assert_eq!(parse_turn("Heartbeat: check your queue"), Turn::default());
    }

    #[test]
    fn channel_display_forms() {
        assert_eq!(channel_uuid(CH).as_deref(), Some(CH));
        assert_eq!(
            channel_uuid(&format!("my (art) (#{CH})")).as_deref(),
            Some(CH)
        );
        assert_eq!(channel_uuid("general"), None);
    }
}
