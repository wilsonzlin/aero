use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

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

    // `ETXTBSY` ("text file busy") can happen on some filesystems if the compiler/linker still has
    // the output file open when we immediately attempt to execute it. Retry a few times with small
    // backoff to make this test robust under parallel `cargo test` runs.
    let output = {
        let mut attempt = 0u32;
        loop {
            match Command::new(&out_path).output() {
                Ok(output) => break output,
                Err(err)
                    if err.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 10 =>
                {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(5 * attempt as u64));
                    continue;
                }
                Err(err) => panic!("failed to run compiled helper: {err}"),
            }
        }
    };
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Best-effort cleanup: avoid leaving compiled helpers behind in /tmp on CI.
    let _ = std::fs::remove_file(&out_path);
}
