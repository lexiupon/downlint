use crate::parser::cst::{
    CstElement, EncodedNode, MdLink, MdLinkDef, Node, OccurrenceId, Tag, TextNode, WikiLink,
};
use crate::utils::ByteRange;

#[derive(Clone, Debug, Default)]
pub struct ScanOutput {
    pub elements: Vec<CstElement>,
    pub masks: Vec<ByteRange>,
    pub frontmatter: Option<TextNode>,
}

pub fn scan_document(input: &str, next_id: &mut OccurrenceId) -> ScanOutput {
    let (masks, frontmatter) = build_masks(input);
    let mut elements = scan_link_definitions(input, next_id);
    let mut occupied: Vec<ByteRange> = elements
        .iter()
        .map(|element| match element {
            CstElement::MLD(node) => node.range,
            _ => ByteRange::new(0, 0),
        })
        .collect();

    let bytes = input.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if is_masked(idx, &masks) || occupied.iter().any(|range| range.contains(idx)) {
            idx += 1;
            continue;
        }

        if let Some((element, end)) = try_scan_wikilink(input, idx, next_id) {
            occupied.push(element_range(&element));
            elements.push(element);
            idx = end;
            continue;
        }
        if let Some((element, end)) = try_scan_markdown_link(input, idx, next_id) {
            occupied.push(element_range(&element));
            elements.push(element);
            idx = end;
            continue;
        }
        idx += 1;
    }

    let link_ranges: Vec<ByteRange> = occupied
        .into_iter()
        .filter(|range| !range.is_empty())
        .collect();
    elements.extend(scan_tags(input, &masks, &link_ranges, next_id));
    elements.sort_by_key(element_start);

    ScanOutput {
        elements,
        masks,
        frontmatter,
    }
}

fn build_masks(input: &str) -> (Vec<ByteRange>, Option<TextNode>) {
    let mut masks = Vec::new();
    let mut frontmatter = None;
    let mut offset = 0usize;
    let mut lines = input.split_inclusive(['\n', '\r']);
    let first_line = lines.next().unwrap_or("");
    if first_line.trim_end_matches(['\n', '\r']).trim() == "---" {
        let mut current = first_line.len();
        for line in lines.by_ref() {
            current += line.len();
            if line.trim_end_matches(['\n', '\r']).trim() == "---" {
                let range = ByteRange::new(0, current);
                frontmatter = Some(TextNode {
                    text: input[range.start..range.end].to_string(),
                    range,
                });
                masks.push(range);
                offset = current;
                break;
            }
        }
    }

    let mut in_fence = false;
    let mut fence_start = 0usize;
    let mut fence_marker = "";
    for line in input[offset..].split_inclusive(['\n', '\r']) {
        let trimmed = line.trim_start();
        if !in_fence && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
            in_fence = true;
            fence_start = offset;
            fence_marker = if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            };
        } else if in_fence && trimmed.starts_with(fence_marker) {
            let end = offset + line.len();
            masks.push(ByteRange::new(fence_start, end));
            in_fence = false;
        }
        offset += line.len();
    }

    (masks, frontmatter)
}

fn scan_link_definitions(input: &str, next_id: &mut OccurrenceId) -> Vec<CstElement> {
    let mut elements = Vec::new();
    let mut offset = 0usize;

    for line in input.split_inclusive(['\n', '\r']) {
        let content = line.trim_end_matches(['\n', '\r']);
        let trimmed = content.trim_start();
        let indent = content.len() - trimmed.len();
        if let Some(stripped) = trimmed.strip_prefix('[')
            && let Some(close) = stripped.find("]:")
        {
            let label = &stripped[..close];
            let rest = stripped[(close + 2)..].trim_start();
            let url_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let url = &rest[..url_end];
            if !label.is_empty() && !url.is_empty() {
                let label_start = offset + indent + 1;
                let label_end = label_start + label.len();
                let url_start = offset + line.find(url).unwrap_or(0);
                let url_end_abs = url_start + url.len();
                let node = Node {
                    id: take_id(next_id),
                    text: content.to_string(),
                    range: ByteRange::new(offset + indent, offset + indent + trimmed.len()),
                    data: MdLinkDef {
                        label: TextNode {
                            text: label.to_string(),
                            range: ByteRange::new(label_start, label_end),
                        },
                        url: EncodedNode {
                            raw: url.to_string(),
                            decoded: decode_component(url),
                            range: ByteRange::new(url_start, url_end_abs),
                        },
                        title: None,
                    },
                };
                elements.push(CstElement::MLD(node));
            }
        }
        offset += line.len();
    }

    elements
}

fn try_scan_wikilink(
    input: &str,
    start: usize,
    next_id: &mut OccurrenceId,
) -> Option<(CstElement, usize)> {
    let bytes = input.as_bytes();
    let (is_embed, open_len, full_start) =
        match (bytes.get(start), bytes.get(start + 1), bytes.get(start + 2)) {
            (Some(b'!'), Some(b'['), Some(b'[')) => (true, 3usize, start),
            (Some(b'['), Some(b'['), _) => (false, 2usize, start),
            _ => return None,
        };
    let content_start = start + open_len;
    let mut idx = content_start;
    while idx + 1 < bytes.len() {
        if bytes[idx] == b'\\' {
            idx += 2;
            continue;
        }
        if bytes[idx] == b']' && bytes[idx + 1] == b']' {
            let (_target_raw, title_raw, target_range, title_range) =
                split_wiki_title(input, content_start, idx);
            let (doc_raw, heading_raw, doc_range, heading_range) =
                split_wiki_heading(input, target_range.start, target_range.end);

            let doc = if doc_range.is_empty() {
                None
            } else {
                Some(EncodedNode {
                    raw: doc_raw.to_string(),
                    decoded: decode_component(doc_raw),
                    range: doc_range,
                })
            };
            let heading = if heading_range.is_empty() {
                None
            } else {
                Some(EncodedNode {
                    raw: heading_raw.to_string(),
                    decoded: decode_component(heading_raw),
                    range: heading_range,
                })
            };
            let title = if title_range.is_empty() {
                None
            } else {
                Some(EncodedNode {
                    raw: title_raw.to_string(),
                    decoded: decode_component(title_raw),
                    range: title_range,
                })
            };
            let node = Node {
                id: take_id(next_id),
                text: input[full_start..idx + 2].to_string(),
                range: ByteRange::new(full_start, idx + 2),
                data: WikiLink {
                    doc,
                    heading,
                    title,
                    is_embed,
                    doc_range: (!doc_range.is_empty()).then_some(doc_range),
                    heading_range: (!heading_range.is_empty()).then_some(heading_range),
                    title_range: (!title_range.is_empty()).then_some(title_range),
                },
            };
            return Some((CstElement::WL(node), idx + 2));
        }
        idx += 1;
    }

    None
}

fn try_scan_markdown_link(
    input: &str,
    start: usize,
    next_id: &mut OccurrenceId,
) -> Option<(CstElement, usize)> {
    let bytes = input.as_bytes();
    let is_image = bytes.get(start) == Some(&b'!') && bytes.get(start + 1) == Some(&b'[');
    let bracket_start = if is_image { start + 1 } else { start };
    if bytes.get(bracket_start) != Some(&b'[') {
        return None;
    }

    let label_end = find_unescaped(input, bracket_start + 1, ']')?;
    let label = &input[bracket_start + 1..label_end];
    let full_start = start;
    let label_range = ByteRange::new(bracket_start + 1, label_end);
    let next = bytes.get(label_end + 1)?;

    if *next == b'(' {
        let dest_end = find_unescaped(input, label_end + 2, ')')?;
        let dest_text = input[label_end + 2..dest_end].trim();
        if dest_text.is_empty() {
            return None;
        }
        let dest_token = dest_text
            .split_whitespace()
            .next()
            .unwrap_or(dest_text)
            .trim_matches(['<', '>']);
        let dest_start = input[label_end + 2..dest_end]
            .find(dest_token)
            .map(|offset| label_end + 2 + offset)
            .unwrap_or(label_end + 2);
        let dest_range = ByteRange::new(dest_start, dest_start + dest_token.len());
        let anchor_range = dest_token
            .find('#')
            .map(|anchor| ByteRange::new(dest_start + anchor + 1, dest_start + dest_token.len()));
        let node = Node {
            id: take_id(next_id),
            text: input[full_start..dest_end + 1].to_string(),
            range: ByteRange::new(full_start, dest_end + 1),
            data: MdLink::Inline {
                text: TextNode {
                    text: label.to_string(),
                    range: label_range,
                },
                dest: TextNode {
                    text: dest_token.to_string(),
                    range: dest_range,
                },
                title: None,
                anchor_range,
                is_image,
            },
        };
        return Some((CstElement::ML(node), dest_end + 1));
    }

    if *next == b'[' {
        let second_end = find_unescaped(input, label_end + 2, ']')?;
        let second = &input[label_end + 2..second_end];
        let data = if second.is_empty() {
            MdLink::Collapsed {
                label: TextNode {
                    text: label.to_string(),
                    range: label_range,
                },
            }
        } else {
            MdLink::Full {
                text: TextNode {
                    text: label.to_string(),
                    range: label_range,
                },
                label: TextNode {
                    text: second.to_string(),
                    range: ByteRange::new(label_end + 2, second_end),
                },
            }
        };
        let node = Node {
            id: take_id(next_id),
            text: input[full_start..second_end + 1].to_string(),
            range: ByteRange::new(full_start, second_end + 1),
            data,
        };
        return Some((CstElement::ML(node), second_end + 1));
    }

    if !label.contains('\n') && !label.contains('\r') {
        let node = Node {
            id: take_id(next_id),
            text: input[full_start..label_end + 1].to_string(),
            range: ByteRange::new(full_start, label_end + 1),
            data: MdLink::Shortcut {
                label: TextNode {
                    text: label.to_string(),
                    range: label_range,
                },
            },
        };
        return Some((CstElement::ML(node), label_end + 1));
    }

    None
}

fn scan_tags(
    input: &str,
    masks: &[ByteRange],
    link_ranges: &[ByteRange],
    next_id: &mut OccurrenceId,
) -> Vec<CstElement> {
    let mut tags = Vec::new();
    let bytes = input.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] != b'#'
            || is_masked(idx, masks)
            || link_ranges.iter().any(|range| range.contains(idx))
        {
            idx += 1;
            continue;
        }
        if is_heading_open(input, idx) {
            idx += 1;
            continue;
        }
        let left_ok = idx == 0 || !is_tag_char(bytes[idx.saturating_sub(1)] as char);
        if !left_ok {
            idx += 1;
            continue;
        }
        let mut end = idx + 1;
        let mut seen = false;
        while end < bytes.len() {
            let ch = bytes[end] as char;
            if is_tag_char(ch) || ch == '/' {
                seen = true;
                end += 1;
            } else {
                break;
            }
        }
        if !seen {
            idx += 1;
            continue;
        }
        let name = &input[idx..end];
        tags.push(CstElement::T(Node {
            id: take_id(next_id),
            text: name.to_string(),
            range: ByteRange::new(idx, end),
            data: Tag {
                name: TextNode {
                    text: name.to_string(),
                    range: ByteRange::new(idx, end),
                },
            },
        }));
        idx = end;
    }
    tags
}

fn split_wiki_title(input: &str, start: usize, end: usize) -> (&str, &str, ByteRange, ByteRange) {
    let content = &input[start..end];
    let mut escaped = false;
    for (offset, ch) in content.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '|' => {
                let target_end = start + offset;
                return (
                    &input[start..target_end],
                    &input[target_end + 1..end],
                    ByteRange::new(start, target_end),
                    ByteRange::new(target_end + 1, end),
                );
            }
            _ => {}
        }
    }
    (
        content,
        "",
        ByteRange::new(start, end),
        ByteRange::new(end, end),
    )
}

fn split_wiki_heading(input: &str, start: usize, end: usize) -> (&str, &str, ByteRange, ByteRange) {
    let content = &input[start..end];
    let mut escaped = false;
    for (offset, ch) in content.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '#' => {
                let doc_end = start + offset;
                return (
                    &input[start..doc_end],
                    &input[doc_end + 1..end],
                    ByteRange::new(start, doc_end),
                    ByteRange::new(doc_end + 1, end),
                );
            }
            _ => {}
        }
    }
    (
        content,
        "",
        ByteRange::new(start, end),
        ByteRange::new(end, end),
    )
}

fn decode_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::new();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] == b'%' && idx + 2 < bytes.len() {
            let hi = bytes[idx + 1] as char;
            let lo = bytes[idx + 2] as char;
            if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() {
                let value = u8::from_str_radix(&format!("{hi}{lo}"), 16).unwrap_or(b'?');
                out.push(value as char);
                idx += 3;
                continue;
            }
        }
        if bytes[idx] == b'\\' && idx + 1 < bytes.len() {
            out.push(bytes[idx + 1] as char);
            idx += 2;
            continue;
        }
        out.push(bytes[idx] as char);
        idx += 1;
    }
    out
}

fn find_unescaped(input: &str, start: usize, needle: char) -> Option<usize> {
    let mut escaped = false;
    for (offset, ch) in input[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == needle {
            return Some(start + offset);
        }
    }
    None
}

fn is_heading_open(input: &str, idx: usize) -> bool {
    let line_start = input[..idx].rfind('\n').map(|value| value + 1).unwrap_or(0);
    input[line_start..idx].trim().is_empty() && input[idx..].starts_with("# ")
}

fn is_tag_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

fn is_masked(offset: usize, masks: &[ByteRange]) -> bool {
    masks.iter().any(|range| range.contains(offset))
}

fn element_range(element: &CstElement) -> ByteRange {
    match element {
        CstElement::WL(node) => node.range,
        CstElement::ML(node) => node.range,
        CstElement::MLD(node) => node.range,
        CstElement::T(node) => node.range,
        CstElement::H(node) => node.range,
        CstElement::Yml(node) => node.range,
    }
}

fn element_start(element: &CstElement) -> usize {
    element_range(element).start
}

fn take_id(next_id: &mut OccurrenceId) -> OccurrenceId {
    let value = *next_id;
    *next_id += 1;
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_basic_wikilink_and_tag() {
        let mut next = 1;
        let output = scan_document("# Title\n[[doc#head|Name]] #rust", &mut next);
        assert!(
            output
                .elements
                .iter()
                .any(|element| matches!(element, CstElement::WL(_)))
        );
        assert!(
            output
                .elements
                .iter()
                .any(|element| matches!(element, CstElement::T(_)))
        );
    }
}
