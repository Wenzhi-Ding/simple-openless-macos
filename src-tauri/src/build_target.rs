pub(crate) fn classify_target_os(target: &str, cargo_cfg_target_os: Option<&str>) -> &'static str {
    match cargo_cfg_target_os {
        Some("macos") => "macos",
        Some(_) => "",
        None if target.ends_with("-apple-darwin") => "macos",
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::classify_target_os;

    #[test]
    fn classifies_only_macos_targets() {
        assert_eq!(
            classify_target_os("aarch64-apple-darwin", Some("macos")),
            "macos"
        );
        assert_eq!(
            classify_target_os("x86_64-unknown-linux-gnu", Some("linux")),
            ""
        );
        assert_eq!(classify_target_os("aarch64-apple-darwin", None), "macos");
    }
}
