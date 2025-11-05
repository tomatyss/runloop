use std::collections::HashSet;
use std::env;
use std::fs;

/// Discover executable commands available on the current PATH.
pub fn path_commands() -> HashSet<String> {
    let mut commands = HashSet::new();
    let Some(path_var) = env::var_os("PATH") else {
        return commands;
    };

    for entry in env::split_paths(&path_var) {
        if let Ok(dir) = fs::read_dir(&entry) {
            for candidate in dir.flatten() {
                if let Some(name) = candidate.file_name().to_str() {
                    commands.insert(name.to_ascii_lowercase());
                }
            }
        }
    }

    commands
}
