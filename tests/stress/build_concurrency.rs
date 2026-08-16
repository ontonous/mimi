// Parallel `mimi build` regression: builds sharing one output directory must
// not race on a fixed `libmimi_runtime.a` archive. Each build uses its own
// per-build temp directory for the runtime archive.
use std::fs;
use std::process::{Command, Stdio};

use super::{mimi_bin, project_root, temp_dir};

#[test]
fn stress_parallel_mimi_build_no_archive_race() {
    // Use a fresh temp directory as the common output directory.
    let dir = temp_dir();
    let src = dir.join("race_case.mimi");
    fs::write(&src, "func main() -> i32 { println(\"race-ok\") 0 }\n").expect("write race source");

    let bin_paths: Vec<_> = (0..4).map(|i| dir.join(format!("race_bin_{i}"))).collect();

    let mut children = Vec::new();
    for out in &bin_paths {
        children.push(
            Command::new(mimi_bin())
                .current_dir(project_root())
                .arg("build")
                .arg(&src)
                .arg("-o")
                .arg(out)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn parallel mimi build"),
        );
    }
    for mut child in children {
        let status = child.wait().expect("wait parallel mimi build");
        assert!(status.success(), "one parallel mimi build failed");
    }

    for out in &bin_paths {
        let output = Command::new(out).output().expect("run race-built binary");
        assert!(output.status.success(), "race-built binary failed: {out:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("race-ok"), "unexpected output: {stdout}");
    }

    let _ = fs::remove_dir_all(&dir);
}
