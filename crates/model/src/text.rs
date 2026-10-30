//! Text from WSF's HTML: tags dropped, entities decoded, spaces
//! collapsed. Shown as text, never as markup.

/// HTML (such as WSF's notices) as plain text: tags dropped, entities decoded,
/// spaces collapsed, one paragraph (or list item) per block, blank lines
/// between. It's shown as text, never as markup.
pub fn plain_text(html: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut block = String::new();
    let mut rest = html;
    let mut skipping = false;
    let mut flush = |block: &mut String| {
        let text = collapse_spaces(&decode_entities(block));
        if !text.is_empty() && text != "•" {
            blocks.push(text);
        }
        block.clear();
    };
    while let Some(open) = rest.find('<') {
        if !skipping {
            block.push_str(&rest[..open]);
        }
        let Some(close) = rest[open..].find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        match name.as_str() {
            "script" | "style" => skipping = !closing,
            "p" | "div" | "ul" | "ol" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "br" => {
                flush(&mut block)
            }
            "li" => {
                flush(&mut block);
                if !closing {
                    block.push_str("• ");
                }
            }
            _ => {}
        }
    }
    if !skipping {
        block.push_str(rest);
    }
    flush(&mut block);
    blocks.join("\n\n")
}

/// Decodes the HTML entities WSF's text uses: the markup ones, spaces
/// and typographic quotes by name, and any by number.
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                "rsquo" => Some('’'),
                "lsquo" => Some('‘'),
                "rdquo" => Some('”'),
                "ldquo" => Some('“'),
                "ndash" => Some('–'),
                "mdash" => Some('—'),
                "hellip" => Some('…'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn collapse_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_html_plain() {
        let html = "<p><span data-x=\"{&quot;1&quot;:0}\">We’re running\r\n 25&nbsp;minutes \
                    <b>behind</b>.</span><br /><br /></p><p></p><ul><li>Check the \
                    <a href=\"https://wsdot.wa.gov\">schedule</a>.</li><li>Arrive early \
                    &#8212; or &#x2014; not.</li></ul><style>p{}</style><p>1 &lt; 2 &amp; R&D</p>";
        assert_eq!(
            plain_text(html),
            "We’re running 25 minutes behind.\n\n• Check the schedule.\n\n\
             • Arrive early — or — not.\n\n1 < 2 & R&D"
        );
        assert_eq!(
            collapse_spaces(&decode_entities(" Older &amp;  settled ")),
            "Older & settled"
        );
        assert_eq!(decode_entities("&bogus; &#xZZ;"), "&bogus; &#xZZ;");
    }
}
