use std::{
    collections::{HashSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
};

const MAX_SEARCH_DEPTH: usize = 4;

pub fn bundled_binary_names(stem: &str, uses_exe_suffix: bool, target_triple: &str) -> Vec<String> {
    let mut names = Vec::new();
    if uses_exe_suffix {
        names.push(format!("{stem}.exe"));
    }
    names.push(stem.to_string());

    if !target_triple.is_empty() {
        if uses_exe_suffix {
            names.push(format!("{stem}-{target_triple}.exe"));
        }
        names.push(format!("{stem}-{target_triple}"));
    }

    names.sort();
    names.dedup();
    names
}

pub fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o111 != 0;
    }

    #[allow(unreachable_code)]
    true
}

pub fn locate_bundled_binary(
    stem: &str,
    env_var: &str,
    uses_exe_suffix: bool,
    target_triple: &str,
) -> Option<PathBuf> {
    let env_override = env::var(env_var)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(path) = env_override.filter(|path| is_executable_file(path)) {
        return Some(path);
    }

    locate_bundled_binary_in_layout(
        &bundled_binary_names(stem, uses_exe_suffix, target_triple),
        env::current_exe().ok().as_deref(),
        env::current_dir().ok().as_deref(),
    )
}

pub fn bundled_binary_candidate_paths(
    names: &[String],
    current_exe: Option<&Path>,
    current_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for seed in search_seed_dirs(current_exe, current_dir) {
        for relative_dir in candidate_relative_dirs() {
            let base = if relative_dir.is_empty() {
                seed.clone()
            } else {
                seed.join(relative_dir)
            };
            for name in names {
                let candidate = base.join(name);
                if seen.insert(candidate.clone()) {
                    candidates.push(candidate);
                }
            }
        }
    }

    candidates
}

pub fn locate_bundled_binary_in_layout(
    names: &[String],
    current_exe: Option<&Path>,
    current_dir: Option<&Path>,
) -> Option<PathBuf> {
    let direct_candidates = bundled_binary_candidate_paths(names, current_exe, current_dir);
    if let Some(found) = direct_candidates
        .iter()
        .find(|candidate| is_executable_file(candidate))
        .cloned()
    {
        return Some(found);
    }

    let recursive_search_roots = search_seed_dirs(current_exe, current_dir);
    for root in recursive_search_roots {
        if let Some(found) = find_binary_recursively(&root, names, MAX_SEARCH_DEPTH) {
            return Some(found);
        }
    }

    None
}

fn search_seed_dirs(current_exe: Option<&Path>, current_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut seeds = Vec::new();
    let mut seen = HashSet::new();

    if let Some(exe) = current_exe {
        if let Some(exe_dir) = exe.parent() {
            push_path_ancestors(exe_dir, 5, &mut seeds, &mut seen);
        }
    }

    if let Some(cwd) = current_dir {
        push_path_ancestors(cwd, 3, &mut seeds, &mut seen);
    }

    seeds
}

fn push_path_ancestors(
    start: &Path,
    levels: usize,
    output: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) {
    let mut current = Some(start);
    for _ in 0..levels {
        let Some(path) = current else {
            break;
        };
        let owned = path.to_path_buf();
        if seen.insert(owned.clone()) {
            output.push(owned);
        }
        current = path.parent();
    }
}

fn candidate_relative_dirs() -> &'static [&'static str] {
    &[
        "",
        "bin",
        "binaries",
        "resources",
        "resources/bin",
        "resources/binaries",
        "Resources",
        "Resources/bin",
        "Resources/binaries",
        "usr/bin",
        "usr/lib",
        "usr/lib/binaries",
        "lib",
        "lib/binaries",
    ]
}

fn find_binary_recursively(root: &Path, names: &[String], max_depth: usize) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }

    let wanted: HashSet<&str> = names.iter().map(String::as_str).collect();
    let allowed_dir_names = [
        "bin",
        "binaries",
        "resources",
        "Resources",
        "lib",
        "usr",
        "MacOS",
        "Frameworks",
    ];

    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = HashSet::new();

    while let Some((dir, depth)) = queue.pop_front() {
        if !visited.insert(dir.clone()) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();

            if wanted.contains(file_name.as_ref()) && is_executable_file(&path) {
                return Some(path);
            }

            if depth >= max_depth || !path.is_dir() {
                continue;
            }

            let should_descend = depth == 0
                || allowed_dir_names
                    .iter()
                    .any(|allowed| file_name.eq_ignore_ascii_case(allowed))
                || names.iter().any(|name| file_name.contains(name));
            if should_descend {
                queue.push_back((path, depth + 1));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{
        bundled_binary_candidate_paths, bundled_binary_names, locate_bundled_binary_in_layout,
    };
    use std::{fs, path::PathBuf};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "noland-managed-binaries-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_executable(path: &PathBuf) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).unwrap();
        }
    }

    #[test]
    fn generates_ci_aligned_binary_names() {
        let names = bundled_binary_names("gotatun", false, "x86_64-unknown-linux-gnu");
        assert!(names.contains(&"gotatun".to_string()));
        assert!(names.contains(&"gotatun-x86_64-unknown-linux-gnu".to_string()));
    }

    #[test]
    fn finds_binary_in_depackaged_resource_layout() {
        let root = temp_root("depackaged");
        let current_exe = root
            .join("Noland Connect.AppDir")
            .join("usr")
            .join("bin")
            .join("noland-connect");
        let binary = root
            .join("Noland Connect.AppDir")
            .join("usr")
            .join("lib")
            .join("noland-connect")
            .join("resources")
            .join("binaries")
            .join("gotatun-x86_64-unknown-linux-gnu");
        write_executable(&binary);

        let names = bundled_binary_names("gotatun", false, "x86_64-unknown-linux-gnu");
        let found = locate_bundled_binary_in_layout(&names, Some(&current_exe), Some(&root));
        assert_eq!(found.as_deref(), Some(binary.as_path()));
    }

    #[test]
    fn includes_direct_candidates_for_known_layouts() {
        let root = temp_root("candidates");
        let current_exe = root.join("bundle").join("MacOS").join("noland-connect");
        let names = bundled_binary_names("ssh", false, "aarch64-apple-darwin");
        let candidates = bundled_binary_candidate_paths(&names, Some(&current_exe), Some(&root));
        assert!(candidates
            .iter()
            .any(|path| { path.ends_with("bundle/Resources/binaries/ssh-aarch64-apple-darwin") }));
    }
}
