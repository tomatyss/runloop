pub mod config;

#[cfg(test)]
mod tests {
    use super::config::Config;

    #[test]
    fn version_defaults_to_one() {
        let cfg = Config::default();
        assert_eq!(cfg.version, 1);
    }
}
