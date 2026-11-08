//! Text that is safe to print: endpoints without credentials, and bounded text from a node
//! or collector.

/// `url` with only its scheme, host and port, for example `wss://node.example:443`. User name,
/// password, path, query and fragment are dropped: any of them can hold an API key. A URL with
/// an `@` after its host, or without a valid scheme, is hidden whole.
pub fn endpoint(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "(URL hidden)".into();
    };
    // Without a scheme, a later `://` (in a query or a password) would show what came before it.
    if !scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return "(URL hidden)".into();
    }
    let (authority, after) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    // A raw `/`, `?` or `#` in a password ends the authority early. With an `@` after it, what
    // came before may be credentials, so nothing is shown.
    if after.contains('@') {
        return "(URL hidden)".into();
    }
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{host}")
}

/// Most characters of text from a node or collector that are printed.
pub const MAX_UNTRUSTED_CHARS: usize = 512;

/// Text from a node or collector, cut to [`MAX_UNTRUSTED_CHARS`] characters, with control
/// characters such as terminal escapes and line breaks replaced by spaces.
pub fn untrusted(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars
        .by_ref()
        .take(MAX_UNTRUSTED_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_keep_only_scheme_host_and_port() {
        for (url, shown) in [
            (
                "wss://user:secret@node.example:443/key?apikey=k#f",
                "wss://node.example:443",
            ),
            ("https://node.example/v1/abcdef", "https://node.example"),
            ("http://127.0.0.1:4318", "http://127.0.0.1:4318"),
            ("http://[::1]:9944?token=t", "http://[::1]:9944"),
            ("secret@node", "(URL hidden)"),
            ("https://user:pa/ss@node.example/x", "(URL hidden)"),
            ("https://user:pa?ss@node.example", "(URL hidden)"),
            ("https://user:pa#ss@node.example", "(URL hidden)"),
            ("https://node.example/x@y", "(URL hidden)"),
            ("https://user:pass@node.example/x", "https://node.example"),
            ("user:pass@node/x?u=http://y", "(URL hidden)"),
            ("admin:se://cret@node", "(URL hidden)"),
        ] {
            assert_eq!(endpoint(url), shown);
        }
        let long = "é\u{1b}[31m".repeat(200);
        let cut = untrusted(&long);
        assert_eq!(cut.chars().count(), MAX_UNTRUSTED_CHARS + 1);
        assert!(cut.ends_with('…') && !cut.contains('\u{1b}'));
    }
}
