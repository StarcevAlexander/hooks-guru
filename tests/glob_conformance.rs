use hooks_guru::Glob;
use serde_json::Value as Json;

#[test]
fn matches_picomatch() {
    let data: Json = serde_json::from_str(include_str!("glob_conformance.json")).unwrap();
    let mut checked = 0;
    let mut failures = Vec::new();
    for row in data.as_array().unwrap() {
        let pattern = row["pattern"].as_str().unwrap();
        let glob = Glob::new(pattern);
        if let Some(expected) = row.get("error") {
            let message = glob
                .err()
                .map(|error| error.message.to_string())
                .unwrap_or_default();
            if message != expected.as_str().unwrap() {
                failures.push(format!(
                    "{pattern:?}: expected error {expected}, got {message:?}"
                ));
            }
            continue;
        }
        let glob = glob.unwrap_or_else(|error| panic!("{pattern:?}: {error}"));
        for (path, expected) in row["results"].as_object().unwrap() {
            checked += 1;
            let actual = glob.is_match(path);
            if actual != expected.as_bool().unwrap() {
                failures.push(format!(
                    "{pattern:?} vs {path:?}: expected {expected}, got {actual}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches of {checked}:\n{}",
        failures.len(),
        failures
            .iter()
            .take(80)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(checked >= 5000, "glob corpus shrank to {checked}");
}
