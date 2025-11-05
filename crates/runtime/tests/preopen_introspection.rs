use runloop_runtime::{Caps, DebugPreopen};

fn parse_caps(source: &str) -> Caps {
    let value: toml::Value = toml::from_str(source).expect("valid toml");
    Caps::from_policy(&value).expect("caps parse").caps
}

#[test]
fn debug_preopens_distinguish_ro_and_rw() {
    let caps = parse_caps(
        r#"[capabilities]
fs_ro = ["/var/log"]
fs_rw = ["/workspace"]
"#,
    );

    let mut descriptors: Vec<DebugPreopen> = caps.debug_preopens();
    descriptors.sort_by(|a, b| a.root.cmp(&b.root));

    assert_eq!(descriptors.len(), 2);

    let ro = &descriptors[0];
    assert_eq!(ro.root.as_str(), "/var/log");
    assert!(ro.dir_read);
    assert!(!ro.dir_write);
    assert!(!ro.dir_create);
    assert!(ro.file_read);
    assert!(!ro.file_write);
    assert!(!ro.file_create);

    let rw = &descriptors[1];
    assert_eq!(rw.root.as_str(), "/workspace");
    assert!(rw.dir_read);
    assert!(rw.dir_write);
    assert!(rw.dir_create);
    assert!(rw.file_read);
    assert!(rw.file_write);
    assert!(rw.file_create);
}
