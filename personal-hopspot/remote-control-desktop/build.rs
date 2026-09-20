use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=android");
    let Ok(kotlin_dir) = env::var("WRY_ANDROID_KOTLIN_FILES_OUT_DIR") else {
        return;
    };
    let kotlin_dir = PathBuf::from(kotlin_dir);
    let app_main = kotlin_dir
        .ancestors()
        .nth(4)
        .expect("WRY kotlin dir is under app/src/main/kotlin");
    copy_tree(Path::new("android/src"), &app_main.join("java"));
    copy_tree(
        Path::new("android/res/xml"),
        &app_main.join("res").join("xml"),
    );
}

fn copy_tree(src: &Path, dst: &Path) {
    if !src.exists() {
        return;
    }
    for entry in fs::read_dir(src).into_iter().flatten().flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            let _ = fs::create_dir_all(&to);
            copy_tree(&from, &to);
        } else if let Some(parent) = to.parent() {
            let _ = fs::create_dir_all(parent);
            let _ = fs::copy(&from, &to);
        }
    }
}
