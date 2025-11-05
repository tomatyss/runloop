use runloop_core::config::RouterConfig;
use runloop_router::{Route, Router};
use std::fs;
use std::path::PathBuf;

#[test]
fn classifier_meets_corpus_accuracy() {
    let mut config = RouterConfig::default();
    config.fastpath_shell = true;

    let router = Router::with_commands(&config, std::iter::empty::<String>());

    let corpus_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
        .join("router_prompts.csv");
    let data = fs::read_to_string(&corpus_path).expect("read router corpus");

    let mut total = 0usize;
    let mut correct = 0usize;
    let mut mismatches = Vec::new();

    for line in data.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut parts = trimmed.splitn(3, ',');
        let prompt = parts.next().map(str::to_string).expect("prompt column");
        let expected_route = parts
            .next()
            .map(str::to_string)
            .expect("expected_route column");
        let notes = parts.next().map(str::to_string);

        let classification = router.classify(&prompt);
        total += 1;
        let expected = match expected_route.as_str() {
            "shell" => Route::Shell,
            "agent" => Route::Agent,
            other => panic!("unexpected route '{other}' in corpus"),
        };
        if classification.route == expected {
            correct += 1;
        } else {
            mismatches.push((prompt, expected, classification.route, notes));
        }
    }

    assert_eq!(total, 100, "router corpus must contain 100 rows");
    let accuracy = correct as f32 / total as f32;
    if accuracy < 0.98 {
        panic!(
            "accuracy {} below threshold; mismatches: {:?}",
            accuracy, mismatches
        );
    }
}
