//! An interrupted native edit ends the run with exit 130, leaves the file as it was, and removes its copy.
//!
//! This is an integration test, so it runs in a process of its own: the cancel flag is global, and other tests
//! must not see it set. It needs the native-mkv feature and ffmpeg (to make a small MKV with two audio tracks).
#![cfg(feature = "native-mkv")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use mau_core::Mau;
use mau_core::model::{request_cancel, reset_cancel, Config, Event, Sink, Task};

fn make_mkv(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("two.mkv");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=duration=1", "-f", "lavfi",
               "-i", "sine=frequency=880:duration=1", "-map", "0", "-map", "1",
               "-metadata:s:a:0", "language=ita", "-metadata:s:a:1", "language=eng", "-c:a", "aac"])
        .arg(&path)
        .status()
        .ok()?;
    status.success().then_some(path)
}

/// Run the edit with `sink`, and check the outcome: exit 130, the file as it was, and no temporary copy left.
fn run_interrupted(path: &Path, dir: &Path, verbose: bool, sink: Sink) {
    let before = std::fs::read(path).unwrap();
    let config = Config {
        file: path.to_string_lossy().into_owned(),
        tasks: vec![Task::new("set", "lang")],
        default: "eng".into(),
        verbose,
        ..Config::default()
    };
    let end = Mau::new(config, sink).run();
    reset_cancel();
    let results: Vec<(String, String, Vec<String>)> =
        end.results.iter().map(|r| (format!("{:?}", r.status), r.message.clone(), r.changes.clone())).collect();
    assert_eq!(end.code, 130, "an interrupted run exits with 130: {results:?}");
    assert_eq!(std::fs::read(path).unwrap(), before, "the file is as it was");
    let temporary: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".fml-tmp"))
        .collect();
    assert!(temporary.is_empty(), "no temporary copy is left: {temporary:?}");
}

#[test]
fn an_interrupted_native_edit_exits_130_and_leaves_the_file_alone() {
    let dir = std::env::temp_dir().join(format!("mau-interrupt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(path) = make_mkv(&dir) else {
        eprintln!("ffmpeg is not installed: the test was not run");
        return;
    };
    // 1. the stop comes while the native writer is working: the engine announces the write, and the stop
    //    is requested at that moment (after the copy is made, so the copy is what gets removed)
    let announced = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&announced);
    let sink: Sink = Arc::new(move |event: Event| {
        if let Event::Note(text) = event {
            if text.starts_with("Executing:\tnative-mkv write-tracks") && !flag.swap(true, std::sync::atomic::Ordering::SeqCst) {
                request_cancel();
            }
        }
    });
    run_interrupted(&path, &dir, true, sink);
    assert!(announced.load(std::sync::atomic::Ordering::SeqCst), "the native write was reached");

    // 2. the stop comes before anything is done
    request_cancel();
    run_interrupted(&path, &dir, false, Arc::new(|_event: Event| {}));
    let _ = std::fs::remove_dir_all(&dir);
}
