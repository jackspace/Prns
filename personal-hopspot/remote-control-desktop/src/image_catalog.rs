//! Controller-owned firmware image catalog under `~/.reticulum/controller/images`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const INDEX_FILE: &str = "index.json";
const RELEASE_FILE: &str = "release.json";
const LOCAL_BUILD_FILE: &str = "local-build.json";
const BUNDLED_FILE: &str = "bundled.json";
const CURRENT_FILE: &str = "current";
const BY_BOARD: &str = "by-board";
const MANIFEST_FILE: &str = "flash-manifest.json";
const TARGET_FILE: &str = "target.json";
const BUNDLE_META_FILE: &str = "bundle.json";
const FIRMWARE_DIR_NAME: &str = "firmware";

#[cfg(test)]
pub(crate) static CATALOG_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("{0}")]
    Message(String),
    #[error("io: {0}")]
    Io(String),
}

impl From<io::Error> for CatalogError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<serde_json::Error> for CatalogError {
    fn from(error: serde_json::Error) -> Self {
        Self::Message(format!("catalog json: {error}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageProvenance {
    Published,
    LocalBuild,
    /// Shared out-of-band board artifacts (zip/folder import); unsigned.
    Imported,
    /// Shipped beside the Controller install (tree build); unsigned.
    Bundled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogImage {
    pub board_slug: String,
    pub image_id: String,
    pub created_at: String,
    pub provenance: ImageProvenance,
    pub channel: String,
    pub version: String,
    pub manifest_sha256: String,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CatalogIndex {
    #[serde(default)]
    boards: Vec<CatalogBoardIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CatalogBoardIndex {
    slug: String,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    current: Option<String>,
}

/// Resolve the catalog root. Prefer `PRNS_CONTROLLER_IMAGES`, then
/// `PRNS_CONTROLLER_HOME/images`, else `~/.reticulum/controller/images`.
pub fn catalog_root() -> Result<PathBuf, CatalogError> {
    if let Some(path) = std::env::var_os("PRNS_CONTROLLER_IMAGES") {
        return Ok(PathBuf::from(path));
    }
    if let Some(home) = std::env::var_os("PRNS_CONTROLLER_HOME") {
        return Ok(PathBuf::from(home).join("images"));
    }
    let home = dirs_home().ok_or_else(|| {
        CatalogError::Message("could not resolve home directory for the image catalog".to_string())
    })?;
    Ok(home.join(".reticulum").join("controller").join("images"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub fn ensure_catalog() -> Result<PathBuf, CatalogError> {
    let root = catalog_root()?;
    fs::create_dir_all(root.join(BY_BOARD))?;
    let index_path = root.join(INDEX_FILE);
    if !index_path.is_file() {
        write_index(&root, &CatalogIndex::default())?;
    }
    seed_bundled_firmware_once();
    Ok(root)
}

fn seed_bundled_firmware_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SEEDED: AtomicBool = AtomicBool::new(false);
    if SEEDED.swap(true, Ordering::SeqCst) {
        return;
    }
    match seed_bundled_firmware() {
        Ok(0) => {}
        Ok(count) => eprintln!("controller catalog: seeded {count} bundled firmware image(s)"),
        Err(error) => eprintln!("controller catalog: bundled firmware seed skipped: {error}"),
    }
}

/// Locate `firmware/` next to the Controller executable or under macOS Resources.
pub fn bundled_firmware_root() -> Option<PathBuf> {
    let Ok(exe) = std::env::current_exe() else {
        return None;
    };
    let Some(dir) = exe.parent() else {
        return None;
    };
    let mut candidates = vec![dir.join(FIRMWARE_DIR_NAME)];
    if dir.file_name().and_then(|name| name.to_str()) == Some("MacOS") {
        if let Some(contents) = dir.parent() {
            candidates.push(contents.join("Resources").join(FIRMWARE_DIR_NAME));
        }
    }
    // Dev convenience: cwd/firmware when running from a staged package folder.
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(FIRMWARE_DIR_NAME));
    }
    candidates.into_iter().find(|path| path.is_dir())
}

#[derive(Debug, Clone, Deserialize)]
struct BundleMeta {
    #[serde(default)]
    version: String,
    #[serde(default)]
    git_sha: String,
    #[serde(default)]
    boards: Vec<String>,
}

/// Copy each board under the install's `firmware/` into the user catalog.
///
/// Sets `current` when the board has no selection yet, or when the current
/// selection is an older Bundled image (package upgrade). Never overrides
/// Published / LocalBuild / Imported selections.
pub fn seed_bundled_firmware() -> Result<usize, CatalogError> {
    let Some(firmware_root) = bundled_firmware_root() else {
        return Ok(0);
    };
    let meta_path = firmware_root.join(BUNDLE_META_FILE);
    let meta: BundleMeta = if meta_path.is_file() {
        serde_json::from_slice(&fs::read(&meta_path)?)?
    } else {
        BundleMeta {
            version: "0.0.0".into(),
            git_sha: "unknown".into(),
            boards: Vec::new(),
        }
    };
    let board_slugs: Vec<String> = if meta.boards.is_empty() {
        let mut slugs = Vec::new();
        for entry in fs::read_dir(&firmware_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(slug) = name.to_str() else {
                continue;
            };
            if slug.starts_with('.') {
                continue;
            }
            if entry.path().join(TARGET_FILE).is_file() {
                slugs.push(slug.to_owned());
            }
        }
        slugs.sort();
        slugs
    } else {
        meta.boards.clone()
    };

    let version = if meta.version.trim().is_empty() {
        "0.0.0"
    } else {
        meta.version.trim()
    };
    let git_sha = if meta.git_sha.trim().is_empty() {
        "unknown"
    } else {
        meta.git_sha.trim()
    };

    let mut seeded = 0usize;
    for slug in board_slugs {
        let src = firmware_root.join(&slug);
        if !src.join(TARGET_FILE).is_file() {
            continue;
        }
        let existing_current = current_image(&slug).ok().flatten();
        let image = register_bundled_image(&slug, version, git_sha, &src)?;
        seeded += 1;
        let should_select = match existing_current.as_ref() {
            None => true,
            Some(current) if current.provenance == ImageProvenance::Bundled => {
                current.image_id != image.image_id
            }
            Some(_) => false,
        };
        if should_select {
            set_current(&slug, &image.image_id)?;
        }
    }
    Ok(seeded)
}

pub fn list_images(board_slug: &str) -> Result<Vec<CatalogImage>, CatalogError> {
    let root = ensure_catalog()?;
    let board_dir = root.join(BY_BOARD).join(board_slug);
    if !board_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut images = Vec::new();
    for entry in fs::read_dir(&board_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        match load_image_record(&entry.path()) {
            Ok(image) if image.complete => images.push(image),
            _ => {}
        }
    }
    images.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(images)
}

pub fn current_image(board_slug: &str) -> Result<Option<CatalogImage>, CatalogError> {
    let root = ensure_catalog()?;
    let pointer = root.join(BY_BOARD).join(board_slug).join(CURRENT_FILE);
    if !pointer.is_file() {
        let images = list_images(board_slug)?;
        return Ok(images.into_iter().next());
    }
    let image_id = fs::read_to_string(&pointer)?.trim().to_owned();
    if image_id.is_empty() {
        return Ok(None);
    }
    let path = root.join(BY_BOARD).join(board_slug).join(&image_id);
    match load_image_record(&path) {
        Ok(image) if image.complete => Ok(Some(image)),
        Ok(_) => Err(CatalogError::Message(format!(
            "catalog image {board_slug}/{image_id} is incomplete"
        ))),
        Err(error) => Err(error),
    }
}

pub fn current_image_dir(board_slug: &str) -> Result<Option<PathBuf>, CatalogError> {
    let Some(image) = current_image(board_slug)? else {
        return Ok(None);
    };
    let root = catalog_root()?;
    Ok(Some(
        root.join(BY_BOARD).join(board_slug).join(&image.image_id),
    ))
}

pub fn set_current(board_slug: &str, image_id: &str) -> Result<(), CatalogError> {
    let root = ensure_catalog()?;
    let image_dir = root.join(BY_BOARD).join(board_slug).join(image_id);
    let image = load_image_record(&image_dir)?;
    if !image.complete {
        return Err(CatalogError::Message(format!(
            "refusing to select incomplete catalog image {board_slug}/{image_id}"
        )));
    }
    write_current_pointer(&root, board_slug, image_id)?;
    let mut index = read_index(&root)?;
    let board = board_entry(&mut index, board_slug);
    board.current = Some(image_id.to_owned());
    if !board.images.iter().any(|id| id == image_id) {
        board.images.push(image_id.to_owned());
    }
    write_index(&root, &index)?;
    Ok(())
}

/// Atomically promote a candidate-shaped source directory into the catalog.
pub fn register_published_image(
    board_slug: &str,
    version: &str,
    channel: &str,
    src_dir: &Path,
) -> Result<CatalogImage, CatalogError> {
    if !src_dir.is_dir() {
        return Err(CatalogError::Message(format!(
            "source image directory {} is missing",
            src_dir.display()
        )));
    }
    let manifest_path = src_dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Err(CatalogError::Message(
            "source image is missing flash-manifest.json".to_string(),
        ));
    }
    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest_sha256 = sha256_hex(&manifest_bytes);
    let image_id = format!("published-{channel}-{version}");
    let root = ensure_catalog()?;
    let board_dir = root.join(BY_BOARD).join(board_slug);
    fs::create_dir_all(&board_dir)?;
    let final_dir = board_dir.join(&image_id);
    let staging_name = format!(".{image_id}.staging-{}", unique_suffix());
    let staging_dir = board_dir.join(&staging_name);
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)?;
    }
    copy_dir_recursive(src_dir, &staging_dir)?;
    let record = CatalogImage {
        board_slug: board_slug.to_owned(),
        image_id: image_id.clone(),
        created_at: now_rfc3339(),
        provenance: ImageProvenance::Published,
        channel: channel.to_owned(),
        version: version.to_owned(),
        manifest_sha256,
        complete: true,
        git_sha: None,
        worktree_path: None,
    };
    fs::write(
        staging_dir.join(RELEASE_FILE),
        serde_json::to_vec_pretty(&record)?,
    )?;
    // Refuse incomplete: require release.json + manifest before promote.
    if !staging_dir.join(MANIFEST_FILE).is_file() {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(CatalogError::Message(
            "refusing to promote incomplete image (missing flash-manifest.json)".to_string(),
        ));
    }
    if final_dir.exists() {
        fs::remove_dir_all(&final_dir)?;
    }
    fs::rename(&staging_dir, &final_dir)?;
    write_current_pointer(&root, board_slug, &image_id)?;
    let mut index = read_index(&root)?;
    let board = board_entry(&mut index, board_slug);
    if !board.images.iter().any(|id| id == &image_id) {
        board.images.push(image_id.clone());
    }
    board.current = Some(image_id);
    write_index(&root, &index)?;
    Ok(record)
}

/// Atomically promote a developer board-artifact directory into the catalog.
pub fn register_local_image(
    board_slug: &str,
    version: &str,
    git_sha: &str,
    worktree: Option<&Path>,
    src_board_dir: &Path,
) -> Result<CatalogImage, CatalogError> {
    if !src_board_dir.is_dir() {
        return Err(CatalogError::Message(format!(
            "source board artifact directory {} is missing",
            src_board_dir.display()
        )));
    }
    let target_path = src_board_dir.join(TARGET_FILE);
    if !target_path.is_file() {
        return Err(CatalogError::Message(
            "source board artifacts are missing target.json".to_string(),
        ));
    }
    let target_bytes = fs::read(&target_path)?;
    let target_sha256 = sha256_hex(&target_bytes);
    let short_sha = shorten_git_sha(git_sha);
    let image_id = sanitize_image_id(&format!("local-{version}-{short_sha}"));
    let root = ensure_catalog()?;
    let board_dir = root.join(BY_BOARD).join(board_slug);
    fs::create_dir_all(&board_dir)?;
    let final_dir = board_dir.join(&image_id);
    let staging_name = format!(".{image_id}.staging-{}", unique_suffix());
    let staging_dir = board_dir.join(&staging_name);
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)?;
    }
    copy_dir_recursive(src_board_dir, &staging_dir)?;
    let record = CatalogImage {
        board_slug: board_slug.to_owned(),
        image_id: image_id.clone(),
        created_at: now_rfc3339(),
        provenance: ImageProvenance::LocalBuild,
        channel: "local".to_owned(),
        version: version.to_owned(),
        manifest_sha256: target_sha256,
        complete: true,
        git_sha: Some(git_sha.to_owned()),
        worktree_path: worktree.map(|path| path.display().to_string()),
    };
    fs::write(
        staging_dir.join(LOCAL_BUILD_FILE),
        serde_json::to_vec_pretty(&record)?,
    )?;
    // Also write release.json so a single loader can find the record.
    fs::write(
        staging_dir.join(RELEASE_FILE),
        serde_json::to_vec_pretty(&record)?,
    )?;
    if !staging_dir.join(TARGET_FILE).is_file() {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(CatalogError::Message(
            "refusing to promote incomplete local image (missing target.json)".to_string(),
        ));
    }
    if final_dir.exists() {
        fs::remove_dir_all(&final_dir)?;
    }
    fs::rename(&staging_dir, &final_dir)?;
    write_current_pointer(&root, board_slug, &image_id)?;
    let mut index = read_index(&root)?;
    let board = board_entry(&mut index, board_slug);
    if !board.images.iter().any(|id| id == &image_id) {
        board.images.push(image_id.clone());
    }
    board.current = Some(image_id);
    write_index(&root, &index)?;
    Ok(record)
}

/// Promote a shared (out-of-band) board-artifact directory into the catalog.
pub fn register_imported_image(
    board_slug: &str,
    version: &str,
    src_board_dir: &Path,
) -> Result<CatalogImage, CatalogError> {
    if !src_board_dir.is_dir() {
        return Err(CatalogError::Message(format!(
            "source board artifact directory {} is missing",
            src_board_dir.display()
        )));
    }
    let target_path = src_board_dir.join(TARGET_FILE);
    if !target_path.is_file() {
        return Err(CatalogError::Message(
            "source board artifacts are missing target.json".to_string(),
        ));
    }
    let target_bytes = fs::read(&target_path)?;
    let target_sha256 = sha256_hex(&target_bytes);
    let short_sha = shorten_git_sha(&target_sha256);
    let image_id = sanitize_image_id(&format!("import-{version}-{short_sha}"));
    let root = ensure_catalog()?;
    let board_dir = root.join(BY_BOARD).join(board_slug);
    fs::create_dir_all(&board_dir)?;
    let final_dir = board_dir.join(&image_id);
    let staging_name = format!(".{image_id}.staging-{}", unique_suffix());
    let staging_dir = board_dir.join(&staging_name);
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)?;
    }
    copy_dir_recursive(src_board_dir, &staging_dir)?;
    let record = CatalogImage {
        board_slug: board_slug.to_owned(),
        image_id: image_id.clone(),
        created_at: now_rfc3339(),
        provenance: ImageProvenance::Imported,
        channel: "import".to_owned(),
        version: version.to_owned(),
        manifest_sha256: target_sha256,
        complete: true,
        git_sha: None,
        worktree_path: None,
    };
    fs::write(
        staging_dir.join(LOCAL_BUILD_FILE),
        serde_json::to_vec_pretty(&record)?,
    )?;
    fs::write(
        staging_dir.join(RELEASE_FILE),
        serde_json::to_vec_pretty(&record)?,
    )?;
    if !staging_dir.join(TARGET_FILE).is_file() {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(CatalogError::Message(
            "refusing to promote incomplete imported image (missing target.json)".to_string(),
        ));
    }
    if final_dir.exists() {
        fs::remove_dir_all(&final_dir)?;
    }
    fs::rename(&staging_dir, &final_dir)?;
    write_current_pointer(&root, board_slug, &image_id)?;
    let mut index = read_index(&root)?;
    let board = board_entry(&mut index, board_slug);
    if !board.images.iter().any(|id| id == &image_id) {
        board.images.push(image_id.clone());
    }
    board.current = Some(image_id);
    write_index(&root, &index)?;
    Ok(record)
}

/// Promote Controller-install `firmware/<slug>/` artifacts into the catalog.
pub fn register_bundled_image(
    board_slug: &str,
    version: &str,
    git_sha: &str,
    src_board_dir: &Path,
) -> Result<CatalogImage, CatalogError> {
    if !src_board_dir.is_dir() {
        return Err(CatalogError::Message(format!(
            "bundled board artifact directory {} is missing",
            src_board_dir.display()
        )));
    }
    let target_path = src_board_dir.join(TARGET_FILE);
    if !target_path.is_file() {
        return Err(CatalogError::Message(
            "bundled board artifacts are missing target.json".to_string(),
        ));
    }
    let target_bytes = fs::read(&target_path)?;
    let target_sha256 = sha256_hex(&target_bytes);
    let short_sha = shorten_git_sha(git_sha);
    let image_id = sanitize_image_id(&format!("bundled-{version}-{short_sha}"));
    let root = ensure_catalog()?;
    let board_dir = root.join(BY_BOARD).join(board_slug);
    fs::create_dir_all(&board_dir)?;
    let final_dir = board_dir.join(&image_id);
    if final_dir.is_dir() && final_dir.join(TARGET_FILE).is_file() {
        // Idempotent: already staged this exact bundle id.
        return load_image_record(&final_dir);
    }
    let staging_name = format!(".{image_id}.staging-{}", unique_suffix());
    let staging_dir = board_dir.join(&staging_name);
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)?;
    }
    copy_dir_recursive(src_board_dir, &staging_dir)?;
    let record = CatalogImage {
        board_slug: board_slug.to_owned(),
        image_id: image_id.clone(),
        created_at: now_rfc3339(),
        provenance: ImageProvenance::Bundled,
        channel: "bundled".to_owned(),
        version: version.to_owned(),
        manifest_sha256: target_sha256,
        complete: true,
        git_sha: Some(git_sha.to_owned()),
        worktree_path: None,
    };
    fs::write(
        staging_dir.join(BUNDLED_FILE),
        serde_json::to_vec_pretty(&record)?,
    )?;
    fs::write(
        staging_dir.join(RELEASE_FILE),
        serde_json::to_vec_pretty(&record)?,
    )?;
    if !staging_dir.join(TARGET_FILE).is_file() {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(CatalogError::Message(
            "refusing to promote incomplete bundled image (missing target.json)".to_string(),
        ));
    }
    if final_dir.exists() {
        fs::remove_dir_all(&final_dir)?;
    }
    fs::rename(&staging_dir, &final_dir)?;
    let mut index = read_index(&root)?;
    let board = board_entry(&mut index, board_slug);
    if !board.images.iter().any(|id| id == &image_id) {
        board.images.push(image_id.clone());
    }
    write_index(&root, &index)?;
    Ok(record)
}

fn load_image_record(image_dir: &Path) -> Result<CatalogImage, CatalogError> {
    let path = if image_dir.join(RELEASE_FILE).is_file() {
        image_dir.join(RELEASE_FILE)
    } else if image_dir.join(LOCAL_BUILD_FILE).is_file() {
        image_dir.join(LOCAL_BUILD_FILE)
    } else if image_dir.join(BUNDLED_FILE).is_file() {
        image_dir.join(BUNDLED_FILE)
    } else {
        return Err(CatalogError::Message(format!(
            "catalog image {} is missing {RELEASE_FILE}",
            image_dir.display()
        )));
    };
    let record: CatalogImage = serde_json::from_slice(&fs::read(&path)?)?;
    let complete = match record.provenance {
        ImageProvenance::Published => record.complete && image_dir.join(MANIFEST_FILE).is_file(),
        ImageProvenance::LocalBuild | ImageProvenance::Imported | ImageProvenance::Bundled => {
            record.complete && image_dir.join(TARGET_FILE).is_file()
        }
    };
    if !complete {
        return Ok(CatalogImage {
            complete: false,
            ..record
        });
    }
    Ok(record)
}
fn shorten_git_sha(git_sha: &str) -> String {
    let trimmed = git_sha.trim();
    if trimmed.len() >= 12 {
        trimmed[..12].to_owned()
    } else if trimmed.is_empty() {
        "unknown".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn sanitize_image_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '.' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_owned()
}

fn write_current_pointer(
    root: &Path,
    board_slug: &str,
    image_id: &str,
) -> Result<(), CatalogError> {
    let board_dir = root.join(BY_BOARD).join(board_slug);
    fs::create_dir_all(&board_dir)?;
    let pointer = board_dir.join(CURRENT_FILE);
    let tmp = board_dir.join(format!(".current.tmp-{}", unique_suffix()));
    fs::write(&tmp, format!("{image_id}\n"))?;
    fs::rename(tmp, pointer)?;
    Ok(())
}

fn read_index(root: &Path) -> Result<CatalogIndex, CatalogError> {
    let path = root.join(INDEX_FILE);
    if !path.is_file() {
        return Ok(CatalogIndex::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_index(root: &Path, index: &CatalogIndex) -> Result<(), CatalogError> {
    let path = root.join(INDEX_FILE);
    let tmp = root.join(format!(".index.tmp-{}", unique_suffix()));
    fs::write(&tmp, serde_json::to_vec_pretty(index)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn board_entry<'a>(index: &'a mut CatalogIndex, slug: &str) -> &'a mut CatalogBoardIndex {
    if let Some(position) = index.boards.iter().position(|board| board.slug == slug) {
        return &mut index.boards[position];
    }
    index.boards.push(CatalogBoardIndex {
        slug: slug.to_owned(),
        images: Vec::new(),
        current: None,
    });
    index.boards.last_mut().expect("just pushed")
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), CatalogError> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            return Err(CatalogError::Message(format!(
                "refusing to copy non-file entry {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    prns_flash_manifest::sha256_hex(bytes)
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

fn unique_suffix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_catalog(test: impl FnOnce(&Path)) {
        let _guard = CATALOG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let dir = std::env::temp_dir().join(format!("prns-catalog-test-{}", unique_suffix()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp catalog");
        std::env::set_var("PRNS_CONTROLLER_IMAGES", &dir);
        test(&dir);
        std::env::remove_var("PRNS_CONTROLLER_IMAGES");
        let _ = fs::remove_dir_all(&dir);
    }

    fn write_candidate_shaped(src: &Path) {
        fs::create_dir_all(src.join("channels")).unwrap();
        fs::write(src.join("flash-manifest.json"), b"{\"schema\":1}").unwrap();
        fs::write(src.join("minisign.pub"), b"test-key\n").unwrap();
        fs::write(src.join("channels/stable.json"), b"{}").unwrap();
    }

    #[test]
    fn promote_sets_current_and_lists_complete_images() {
        with_temp_catalog(|root| {
            let src = root.join("incoming");
            write_candidate_shaped(&src);
            let image = register_published_image("heltec-v4-r8", "0.3.7", "stable", &src)
                .expect("register");
            assert!(image.complete);
            assert_eq!(image.image_id, "published-stable-0.3.7");
            let current = current_image("heltec-v4-r8")
                .expect("current")
                .expect("some");
            assert_eq!(current.image_id, image.image_id);
            let listed = list_images("heltec-v4-r8").expect("list");
            assert_eq!(listed.len(), 1);
            let dir = current_image_dir("heltec-v4-r8")
                .expect("dir")
                .expect("path");
            assert!(dir.join("flash-manifest.json").is_file());
            assert!(dir.join("release.json").is_file());
        });
    }

    #[test]
    fn incomplete_dirs_are_not_listed_or_selectable() {
        with_temp_catalog(|root| {
            let board = root.join(BY_BOARD).join("heltec-v4");
            let incomplete = board.join("published-stable-9.9.9");
            fs::create_dir_all(&incomplete).unwrap();
            fs::write(
                incomplete.join(RELEASE_FILE),
                serde_json::to_vec(&CatalogImage {
                    board_slug: "heltec-v4".into(),
                    image_id: "published-stable-9.9.9".into(),
                    created_at: "1".into(),
                    provenance: ImageProvenance::Published,
                    channel: "stable".into(),
                    version: "9.9.9".into(),
                    manifest_sha256: "00".into(),
                    complete: false,
                    git_sha: None,
                    worktree_path: None,
                })
                .unwrap(),
            )
            .unwrap();
            assert!(list_images("heltec-v4").unwrap().is_empty());
            assert!(set_current("heltec-v4", "published-stable-9.9.9").is_err());
        });
    }

    #[test]
    fn register_refuses_source_without_manifest() {
        with_temp_catalog(|root| {
            let src = root.join("bad");
            fs::create_dir_all(&src).unwrap();
            assert!(register_published_image("heltec-v4", "0.1.0", "stable", &src).is_err());
        });
    }

    #[test]
    fn register_local_image_sets_current_from_board_artifacts() {
        with_temp_catalog(|root| {
            let src = root.join("board-artifacts");
            fs::create_dir_all(&src).unwrap();
            fs::write(src.join("target.json"), br#"{"board":"heltec-v4-r8"}"#).unwrap();
            fs::write(src.join("application.bin"), b"fw").unwrap();
            let image = register_local_image(
                "heltec-v4-r8",
                "0.3.7",
                "abcdef0123456789",
                Some(Path::new("/repo")),
                &src,
            )
            .expect("register local");
            assert_eq!(image.provenance, ImageProvenance::LocalBuild);
            assert_eq!(image.image_id, "local-0.3.7-abcdef012345");
            assert_eq!(image.git_sha.as_deref(), Some("abcdef0123456789"));
            let current = current_image("heltec-v4-r8").unwrap().unwrap();
            assert_eq!(current.image_id, image.image_id);
            let dir = current_image_dir("heltec-v4-r8").unwrap().unwrap();
            assert!(dir.join("target.json").is_file());
            assert!(dir.join("local-build.json").is_file());
            assert!(dir.join("release.json").is_file());
        });
    }

    #[test]
    fn register_local_refuses_missing_target_json() {
        with_temp_catalog(|root| {
            let src = root.join("empty");
            fs::create_dir_all(&src).unwrap();
            assert!(register_local_image("heltec-v4", "0.1.0", "deadbeef", None, &src).is_err());
        });
    }

    #[test]
    fn register_imported_image_sets_current_from_board_artifacts() {
        with_temp_catalog(|root| {
            let src = root.join("imported-artifacts");
            fs::create_dir_all(&src).unwrap();
            let target = br#"{"board_slug":"heltec-v4-r8"}"#;
            fs::write(src.join("target.json"), target).unwrap();
            fs::write(src.join("application.bin"), b"fw").unwrap();
            let image = register_imported_image("heltec-v4-r8", "0.3.7", &src).expect("import");
            assert_eq!(image.provenance, ImageProvenance::Imported);
            assert_eq!(image.channel, "import");
            assert!(image.image_id.starts_with("import-0.3.7-"));
            assert!(image.git_sha.is_none());
            let current = current_image("heltec-v4-r8").unwrap().unwrap();
            assert_eq!(current.image_id, image.image_id);
            let dir = current_image_dir("heltec-v4-r8").unwrap().unwrap();
            assert!(dir.join("target.json").is_file());
            assert!(dir.join("release.json").is_file());
        });
    }

    #[test]
    fn register_imported_refuses_missing_target_json() {
        with_temp_catalog(|root| {
            let src = root.join("empty");
            fs::create_dir_all(&src).unwrap();
            assert!(register_imported_image("heltec-v4", "0.1.0", &src).is_err());
        });
    }

    #[test]
    fn register_bundled_image_is_idempotent_and_does_not_force_current() {
        with_temp_catalog(|root| {
            let src = root.join("bundled-artifacts");
            fs::create_dir_all(&src).unwrap();
            fs::write(src.join("target.json"), br#"{"board_slug":"heltec-v4"}"#).unwrap();
            fs::write(src.join("application.bin"), b"fw").unwrap();

            let imported_src = root.join("imported-first");
            fs::create_dir_all(&imported_src).unwrap();
            fs::write(
                imported_src.join("target.json"),
                br#"{"board_slug":"heltec-v4"}"#,
            )
            .unwrap();
            fs::write(imported_src.join("application.bin"), b"other").unwrap();
            let imported =
                register_imported_image("heltec-v4", "0.1.0", &imported_src).expect("import");

            let bundled = register_bundled_image("heltec-v4", "0.3.7", "abcdef0123456789", &src)
                .expect("bundle");
            assert_eq!(bundled.provenance, ImageProvenance::Bundled);
            assert_eq!(bundled.channel, "bundled");
            assert!(bundled.image_id.starts_with("bundled-0.3.7-"));

            // Imported selection must win until seed policy selects bundled.
            let current = current_image("heltec-v4").unwrap().unwrap();
            assert_eq!(current.image_id, imported.image_id);

            let again = register_bundled_image("heltec-v4", "0.3.7", "abcdef0123456789", &src)
                .expect("idempotent");
            assert_eq!(again.image_id, bundled.image_id);
        });
    }

    #[test]
    fn seed_bundled_firmware_selects_when_board_empty() {
        with_temp_catalog(|root| {
            let firmware = root.join("install").join("firmware");
            let board = firmware.join("heltec-v4-r8");
            fs::create_dir_all(&board).unwrap();
            fs::write(
                board.join("target.json"),
                br#"{"board_slug":"heltec-v4-r8"}"#,
            )
            .unwrap();
            fs::write(board.join("application.bin"), b"fw").unwrap();
            fs::write(
                firmware.join("bundle.json"),
                br#"{"schema":1,"version":"0.3.7","git_sha":"deadbeefcafebabe","boards":["heltec-v4-r8"]}"#,
            )
            .unwrap();

            // Point the "install" layout via cwd so bundled_firmware_root finds it.
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(root.join("install")).unwrap();
            let seeded = seed_bundled_firmware().expect("seed");
            std::env::set_current_dir(previous).unwrap();
            assert_eq!(seeded, 1);
            let current = current_image("heltec-v4-r8").unwrap().unwrap();
            assert_eq!(current.provenance, ImageProvenance::Bundled);
            assert_eq!(current.version, "0.3.7");
        });
    }
}
