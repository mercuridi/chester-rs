use url::Url;

pub fn get_youtube_id(link: &str) -> Option<String> {
    // Try to parse the URL; bail out if it's invalid
    tracing::debug!("Parsing YouTube link {}", link);
    let url = Url::parse(link).ok()?;
    let host = url.host_str()?;

    match host {
        // Short links: https://youtu.be/VIDEO_ID
        "youtu.be" => {
            // path_segments() -> segments between the slashes
            url.path_segments()
                .and_then(|mut segs| segs.next())
                .map(std::string::ToString::to_string)
        }

        // Standard watch URLs, mobile, or www embeds
        "www.youtube.com" | "youtube.com" | "m.youtube.com" => {
            // 1) /watch?v=VIDEO_ID
            if let Some((_, v)) = url.query_pairs().find(|(k, _)| k == "v") {
                return Some(v.into_owned());
            }
            // 2) /embed/VIDEO_ID
            url.path_segments()
                .and_then(|mut segs| segs.find(|part| *part == "embed").and_then(|_| segs.next()))
                .map(std::string::ToString::to_string)
        }

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::get_youtube_id;

    #[test]
    fn extracts_ids_from_supported_youtube_urls() {
        let cases = [
            ("https://youtu.be/abc123", "abc123"),
            ("https://www.youtube.com/watch?v=abc123", "abc123"),
            ("https://youtube.com/watch?feature=share&v=abc123", "abc123"),
            ("https://m.youtube.com/watch?v=abc123&t=3", "abc123"),
            ("https://www.youtube.com/embed/abc123", "abc123"),
        ];

        for (url, expected) in cases {
            assert_eq!(get_youtube_id(url).as_deref(), Some(expected), "{url}");
        }
    }

    #[test]
    fn rejects_invalid_and_non_youtube_urls() {
        for input in [
            "not a url",
            "abc123",
            "https://example.com/watch?v=abc123",
            "https://youtube.example.com/watch?v=abc123",
        ] {
            assert_eq!(get_youtube_id(input), None, "{input}");
        }
    }

    #[test]
    fn watch_url_without_video_parameter_is_not_a_video() {
        assert_eq!(
            get_youtube_id("https://youtube.com/watch?feature=share"),
            None
        );
    }

    #[test]
    fn short_url_preserves_percent_decoded_path_segment() {
        assert_eq!(
            get_youtube_id("https://youtu.be/a%20b").as_deref(),
            Some("a%20b")
        );
    }
}
