use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn parse_manifest_tests(manifest_text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in manifest_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        let name = line.split_whitespace().next().unwrap_or("");
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

fn strip_cmake_comments(cmake_text: &str) -> String {
    cmake_text
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn cmake_defines_test(cmake_text_without_comments: &str, test_name: &str) -> bool {
    // We intentionally keep this simple (string match) because the project's CMake style uses:
    //   aerogpu_add_win7_test(<name> ...)
    // and we want the regression test to be obvious and dependency-free.
    let needle = format!("aerogpu_add_win7_test({test_name}");
    cmake_text_without_comments.contains(&needle)
}

#[test]
fn win7_guest_tests_manifest_is_in_sync_with_cmake_targets() {
    let root = repo_root();

    let win7_tests_root = root.join("drivers/aerogpu/tests/win7");
    assert!(
        win7_tests_root.is_dir(),
        "expected Win7 guest test directory at {}",
        win7_tests_root.display()
    );

    let manifest_path = win7_tests_root.join("tests_manifest.txt");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest_path.display()));
    let manifest_tests = parse_manifest_tests(&manifest_text);
    assert!(
        !manifest_tests.is_empty(),
        "{} must list at least one test",
        manifest_path.display()
    );

    let cmake_path = win7_tests_root.join("CMakeLists.txt");
    let cmake_text = std::fs::read_to_string(&cmake_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", cmake_path.display()));
    let cmake_text = strip_cmake_comments(&cmake_text);

    let mut missing_cmake_targets = Vec::new();
    let mut missing_vs2010_build_cmds = Vec::new();

    for name in &manifest_tests {
        let test_dir = win7_tests_root.join(name);

        // Manifest explicitly allows placeholders: missing directories are OK.
        if !test_dir.is_dir() {
            continue;
        }

        // Nice-to-have: keep VS2010 build scripts in sync with manifest too. The in-guest
        // build_all_vs2010.cmd hard-fails if a listed test directory exists but lacks this script.
        if !test_dir.join("build_vs2010.cmd").is_file() {
            missing_vs2010_build_cmds.push(test_dir.clone());
        }

        // Only enforce CMake targets for the standard `main.cpp` layout; some tests use custom
        // entry points (e.g. producer/consumer binaries) and are intentionally exempt.
        if !test_dir.join("main.cpp").is_file() {
            continue;
        }

        if !cmake_defines_test(&cmake_text, name) {
            missing_cmake_targets.push(name.clone());
        }
    }

    assert!(
        missing_vs2010_build_cmds.is_empty(),
        "Win7 tests listed in {} must include build_vs2010.cmd when their directory exists.\nMissing:\n{}",
        manifest_path.display(),
        missing_vs2010_build_cmds
            .iter()
            .map(|path| format!(
                "- {}",
                path.strip_prefix(&root).unwrap_or(path).display()
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );

    assert!(
        missing_cmake_targets.is_empty(),
        "Win7 tests listed in {} with an in-tree main.cpp must have a corresponding aerogpu_add_win7_test(...) target in {}.\nMissing CMake targets:\n{}",
        manifest_path.display(),
        cmake_path.display(),
        missing_cmake_targets
            .iter()
            .map(|name| format!(
                "- {name} (expected to find `aerogpu_add_win7_test({name}` in {})",
                cmake_path.strip_prefix(&root).unwrap_or(&cmake_path).display()
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn win7_guest_tests_manifest_uses_unambiguous_names() {
    // Guard against subtle manifest errors (e.g. copy/paste including trailing punctuation) that
    // would make the sync check produce confusing results.
    let root = repo_root();
    let manifest_path = root.join("drivers/aerogpu/tests/win7/tests_manifest.txt");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest_path.display()));
    let tests = parse_manifest_tests(&manifest_text);

    let mut bad = Vec::new();
    for name in tests {
        if name.contains('/') || name.contains('\\') {
            bad.push(name);
        }
    }

    assert!(
        bad.is_empty(),
        "{} contains test names with path separators: {bad:?}",
        manifest_path.display()
    );
}
