//! Toast-template XML: pure string building with XML escaping, so titles
//! and bodies arriving from terminal streams can never break the payload
//! (upstream's banner content flows from agent output — treat as data).
//!
//! The template is the generic one (`ToastGeneric`): attribution line, an
//! optional bold title, an optional body, and an optional mute. Windows
//! shows the app identity from the AUMID shortcut automatically.

/// Escapes text for XML content and attribute positions (we quote every
/// attribute, so `"` must escape too).
pub fn escape_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Builds the generic toast payload. `title` always shows (attributed line
/// apart); `body` is optional; `silent` maps from the policy's sound flag.
pub fn toast_xml(title: &str, body: &str, silent: bool, attribution: &str) -> String {
    let mut out = String::new();
    out.push_str(r#"<toast><visual><binding template="ToastGeneric">"#);
    if !attribution.is_empty() {
        out.push_str(&format!(
            r#"<text placement="attribution">{}</text>"#,
            escape_text(attribution)
        ));
    }
    out.push_str(&format!("<text>{}</text>", escape_text(title)));
    if !body.is_empty() {
        out.push_str(&format!("<text>{}</text>", escape_text(body)));
    }
    out.push_str("</binding></visual>");
    if silent {
        out.push_str(r#"<audio silent="true"/>"#);
    } else {
        out.push_str(r#"<audio src="ms-winsoundevent:Notification.Default"/>"#);
    }
    out.push_str("</toast>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup_and_quotes() {
        assert_eq!(
            escape_text(r#"a<b>& "c" 'd'"#),
            r"a&lt;b&gt;&amp; &quot;c&quot; &apos;d&apos;"
        );
    }

    #[test]
    fn passes_through_utf8() {
        assert_eq!(escape_text("héllo→"), "héllo→");
    }

    #[test]
    fn generic_template_carries_title_and_body() {
        let xml = toast_xml("Task 42", "All checks passed", false, "cmux");
        assert!(
            xml.contains(r#"<text placement="attribution">cmux</text>"#),
            "{xml}"
        );
        assert!(xml.contains("<text>Task 42</text>"), "{xml}");
        assert!(xml.contains("<text>All checks passed</text>"), "{xml}");
        assert!(
            xml.contains(r#"src="ms-winsoundevent:Notification.Default""#),
            "{xml}"
        );
        assert!(!xml.contains("silent"), "{xml}");
    }

    #[test]
    fn silent_toasts_play_no_sound() {
        let xml = toast_xml("Waiting", "", true, "cmux");
        assert!(xml.contains(r#"<audio silent="true"/>"#), "{xml}");
        assert!(!xml.contains("Notification.Default"), "{xml}");
        // No body is fine for ToastGeneric: only the title text node.
        assert_eq!(xml.matches("<text>").count(), 1);
    }

    #[test]
    fn escaped_content_survives_the_template() {
        let xml = toast_xml("a<b>", "x & y \"z\"", true, "cmu<x>");
        assert!(xml.contains("<text>a&lt;b&gt;</text>"), "{xml}");
        assert!(xml.contains(">x &amp; y &quot;z&quot;</text>"), "{xml}");
        assert!(
            xml.contains(r#"placement="attribution">cmu&lt;x&gt;</text>"#),
            "{xml}"
        );
    }
}
