use std::path::PathBuf;
use std::process::Command;

#[test]
fn dbgctl_watch_fence_math_helper() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_dir.join("../..");
    let c_src = crate_dir.join("tests/aerogpu_dbgctl_watch_fence_math.c");

    let mut out_path = std::env::temp_dir().join(format!(
        "aerogpu_dbgctl_watch_fence_math_{}",
        std::process::id()
    ));
    if cfg!(windows) {
        out_path.set_extension("exe");
    }

    let status = Command::new("cc")
        .arg("-I")
        .arg(&repo_root)
        .arg("-std=c11")
        .arg("-O2")
        .arg("-o")
        .arg(&out_path)
        .arg(&c_src)
        .status()
        .expect("failed to spawn C compiler");
    assert!(status.success(), "C compiler failed with status {status}");

    let output = Command::new(&out_path)
        .output()
        .expect("failed to run compiled helper");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
