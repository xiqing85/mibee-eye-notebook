use include_dir::Dir;
use include_dir::include_dir;

static STATIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/static");

/// Get the content of the index.html file
pub fn index_html() -> &'static str {
    STATIC_DIR
        .get_file("index.html")
        .expect("index.html should be embedded")
        .contents_utf8()
        .expect("index.html should be valid UTF-8")
}

/// Get the content of any embedded file by path
pub fn get_file_content(path: &str) -> Option<&'static str> {
    STATIC_DIR
        .get_file(path.trim_start_matches('/'))
        .and_then(|f| f.contents_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_html_contains_title() {
        let html = index_html();
        assert!(
            html.contains("<title>MiBee Cam</title>"),
            "index.html should contain the title tag"
        );
        assert!(
            html.contains("MiBee Cam"),
            "index.html should contain the project name"
        );
    }

    #[test]
    fn static_dir_contains_index() {
        assert!(
            STATIC_DIR.get_file("index.html").is_some(),
            "index.html should be embedded"
        );
    }
}
