//! Shared URL parsing helpers for stream providers.

pub fn strip_query(value: &str) -> &str {
    value.split_once('?').map_or(value, |(head, _)| head)
}

pub fn last_path_segment(url: &str) -> Option<&str> {
    url.split('/')
        .rev()
        .find(|segment| !segment.is_empty() && !segment.starts_with('?'))
}

pub fn segment_after<'a>(url: &'a str, marker: &str) -> Option<&'a str> {
    let mut segments = url.split('/');

    while let Some(segment) = segments.next() {
        if segment == marker {
            return segments.next().map(strip_query);
        }
    }

    None
}

pub fn last_path_segment_without_query(url: &str) -> Option<&str> {
    last_path_segment(url).map(strip_query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_query_suffix() {
        assert_eq!(strip_query("abc123?foo=bar"), "abc123");
        assert_eq!(strip_query("abc123"), "abc123");
    }

    #[test]
    fn finds_last_path_segment() {
        assert_eq!(
            last_path_segment("https://example.com/path/to/item?foo=bar"),
            Some("item?foo=bar")
        );
        assert_eq!(
            last_path_segment("https://example.com/path/to/item"),
            Some("item")
        );
        assert_eq!(
            last_path_segment("https://example.com"),
            Some("example.com")
        );
    }

    #[test]
    fn finds_last_path_segment_without_query() {
        assert_eq!(
            last_path_segment_without_query("https://example.com/path/to/item?foo=bar"),
            Some("item")
        );
        assert_eq!(
            last_path_segment_without_query("https://example.com/path/to/item"),
            Some("item")
        );
        assert_eq!(
            last_path_segment_without_query("https://example.com"),
            Some("example.com")
        );
    }

    #[test]
    fn finds_segment_after_marker() {
        assert_eq!(
            segment_after("https://tv.nrk.no/program/KMTE50001219?foo=bar", "program"),
            Some("KMTE50001219")
        );
        assert_eq!(
            segment_after(
                "https://www.dr.dk/drtv/serie/gintberg-til-gaes_123456",
                "serie"
            ),
            Some("gintberg-til-gaes_123456")
        );
        assert_eq!(
            segment_after("https://example.com/path/to/item", "serie"),
            None
        );
    }
}
