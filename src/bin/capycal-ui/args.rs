//! Command line arguments of the desktop apps.

/// The value of `-c <path>` or `--config <path>`. Other arguments are ignored,
/// since macOS may pass its own when launching an app.
pub fn config_path_from_args(mut args: impl Iterator<Item = String>) -> Option<std::path::PathBuf> {
    while let Some(arg) = args.next() {
        if arg == "-c" || arg == "--config" {
            return args.next().map(Into::into);
        }
        if let Some(path) = arg.strip_prefix("--config=") {
            return Some(path.into());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Option<PathBuf> {
        config_path_from_args(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn test_config_path_from_args() {
        assert_eq!(parse(&["-c", "/a.toml"]), Some("/a.toml".into()));
        assert_eq!(
            parse(&["-psn_0_123", "--config", "b.toml"]),
            Some("b.toml".into())
        );
        assert_eq!(parse(&["--config=/c.toml"]), Some("/c.toml".into()));
        assert_eq!(parse(&["-c"]), None);
        assert_eq!(parse(&[]), None);
    }
}
