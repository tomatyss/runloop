use std::env;
use std::fs;
use std::io;

fn main() -> io::Result<()> {
    let paths = env::args().skip(1).collect::<Vec<_>>();
    if paths.is_empty() {
        eprintln!("usage: b3sum <file> [...]");
        std::process::exit(1);
    }
    for (path, hash) in hash_inputs(paths)? {
        println!("{path}\t{hash}");
    }
    Ok(())
}

fn hash_inputs(mut paths: Vec<String>) -> io::Result<Vec<(String, String)>> {
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no input files provided",
        ));
    }
    paths.sort();
    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = fs::read(&path)?;
        let hash = blake3::hash(&bytes).to_string();
        results.push((path, hash));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn hash_inputs_sorts_and_hashes_consistently() {
        let mut first = NamedTempFile::new().expect("first file");
        writeln!(first, "alpha").expect("write first");
        let mut second = NamedTempFile::new().expect("second file");
        writeln!(second, "beta").expect("write second");
        let path_a = first.path().to_string_lossy().to_string();
        let path_b = second.path().to_string_lossy().to_string();
        let mut expected = vec![path_a.clone(), path_b.clone()];
        expected.sort();
        let results = hash_inputs(vec![path_b.clone(), path_a.clone()]).expect("hash inputs");
        assert_eq!(results.len(), expected.len());
        for (result, expected_path) in results.iter().zip(expected.iter()) {
            assert_eq!(&result.0, expected_path);
            let manual_hash = blake3::hash(&fs::read(&result.0).unwrap()).to_string();
            assert_eq!(result.1, manual_hash);
        }
    }

    #[test]
    fn hash_inputs_errors_on_missing_file() {
        let err = hash_inputs(vec!["/nonexistent/file".into()]).expect_err("missing file");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn hash_inputs_rejects_empty_arguments() {
        let err = hash_inputs(Vec::new()).expect_err("empty args");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
