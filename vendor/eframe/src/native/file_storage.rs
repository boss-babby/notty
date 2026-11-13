use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
};

/// The folder where `eframe` will store its state.
///
/// The given `app_id` is either the
/// [`egui::ViewportBuilder::app_id`] of [`crate::NativeOptions::viewport`]
/// or the title argument to [`crate::run_native`].
///
/// On native, the path is:
/// * Linux:   `/home/UserName/.local/share/APP_ID`
/// * macOS:   `/Users/UserName/Library/Application Support/APP_ID`
/// * Windows: `C:\Users\UserName\AppData\Roaming\APP_ID\data`
pub fn storage_dir(app_id: &str) -> Option<PathBuf> {
    use egui::os::OperatingSystem as OS;
    use std::env::var_os;
    // Android (and sandboxed launchers) hand the app its private data directory through this variable.
    if let Some(dir) = var_os("LUMEN_DATA_DIR").map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(dir.join("storage"));
    }
    match OS::from_target_os() {
        OS::Nix => var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home::home_dir().map(|p| p.join(".local").join("share")))
            .map(|p| {
                p.join(
                    app_id
                        .to_lowercase()
                        .replace(|c: char| c.is_ascii_whitespace(), ""),
                )
            }),
        OS::Mac => home::home_dir().map(|p| {
            p.join("Library")
                .join("Application Support")
                .join(app_id.replace(|c: char| c.is_ascii_whitespace(), "-"))
        }),
        OS::Windows => roaming_appdata().map(|p| p.join(app_id).join("data")),
        OS::Unknown | OS::Android | OS::IOS => None,
    }
}

// Adapted from
// https://github.com/rust-lang/cargo/blob/6e11c77384989726bb4f412a0e23b59c27222c34/crates/home/src/windows.rs#L19-L37
#[cfg(all(windows, not(target_vendor = "uwp")))]
#[allow(unsafe_code)]
fn roaming_appdata() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::ptr;
    use std::slice;

    use windows_sys::Win32::Foundation::S_OK;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DONT_VERIFY,
    };

    extern "C" {
        fn wcslen(buf: *const u16) -> usize;
    }
    let mut path_raw = ptr::null_mut();

    // SAFETY: SHGetKnownFolderPath allocates for us, we don't pass any pointers to it.
    // See https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderpath
    let result = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_RoamingAppData,
            KF_FLAG_DONT_VERIFY as u32,
            std::ptr::null_mut(),
            &mut path_raw,
        )
    };

    let path = if result == S_OK {
        // SAFETY: SHGetKnownFolderPath indicated success and is supposed to allocate a nullterminated string for us.
        let path_slice = unsafe { slice::from_raw_parts(path_raw, wcslen(path_raw)) };
        Some(PathBuf::from(OsString::from_wide(path_slice)))
    } else {
        None
    };

    // SAFETY:
    // This memory got allocated by SHGetKnownFolderPath, we didn't touch anything in the process.
    // A null ptr is a no-op for `CoTaskMemFree`, so in case this failed we're still good.
    // https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-cotaskmemfree
    unsafe { CoTaskMemFree(path_raw.cast()) };

    path
}

#[cfg(any(not(windows), target_vendor = "uwp"))]
fn roaming_appdata() -> Option<PathBuf> {
    None
}

// ----------------------------------------------------------------------------

/// A key-value store backed by a [RON](https://github.com/ron-rs/ron) file on disk.
/// Used to restore egui state, glow window position/size and app state.
pub struct FileStorage {
    ron_filepath: PathBuf,
    kv: HashMap<String, String>,
    dirty: bool,
    last_save_join_handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FileStorage {
    fn drop(&mut self) {
        if let Some(join_handle) = self.last_save_join_handle.take() {
            profiling::scope!("wait_for_save");
            join_handle.join().ok();
        }
    }
}

impl FileStorage {
    /// Store the state in this .ron file.
    pub(crate) fn from_ron_filepath(ron_filepath: impl Into<PathBuf>) -> Self {
        profiling::function_scope!();
        let ron_filepath: PathBuf = ron_filepath.into();
        log::debug!("Loading app state from {:?}…", ron_filepath);
        Self {
            kv: read_with_recovery(&ron_filepath).unwrap_or_default(),
            ron_filepath,
            dirty: false,
            last_save_join_handle: None,
        }
    }

    /// Find a good place to put the files that the OS likes.
    pub fn from_app_id(app_id: &str) -> Option<Self> {
        profiling::function_scope!();
        if let Some(data_dir) = storage_dir(app_id) {
            if let Err(err) = std::fs::create_dir_all(&data_dir) {
                log::warn!(
                    "Saving disabled: Failed to create app path at {:?}: {}",
                    data_dir,
                    err
                );
                None
            } else {
                Some(Self::from_ron_filepath(data_dir.join("app.ron")))
            }
        } else {
            log::warn!("Saving disabled: Failed to find path to data_dir.");
            None
        }
    }
}

impl crate::Storage for FileStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.kv.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        if self.kv.get(key) != Some(&value) {
            self.kv.insert(key.to_owned(), value);
            self.dirty = true;
        }
    }

    fn flush(&mut self) {
        if self.dirty {
            profiling::scope!("FileStorage::flush");
            self.dirty = false;

            let file_path = self.ron_filepath.clone();
            let kv = self.kv.clone();

            if let Some(join_handle) = self.last_save_join_handle.take() {
                // wait for previous save to complete.
                join_handle.join().ok();
            }

            let result = std::thread::Builder::new()
                .name("eframe_persist".to_owned())
                .spawn(move || {
                    save_to_disk(&file_path, &kv);
                });
            match result {
                Ok(join_handle) => {
                    self.last_save_join_handle = Some(join_handle);
                }
                Err(err) => {
                    log::warn!("Failed to spawn thread to save app state: {err}");
                }
            }
        }
    }
}

/// Lumen data safety: the state is written to a temporary file, flushed to the disk and only then renamed over the real file, so a crash,
/// a power cut or a full disk can never leave a half-written `app.ron` behind. The previous good file is kept as `app.ron.bak` and a
/// rolling set of hourly snapshots lives in `backups/` (see `read_with_recovery` for how they are used).
fn save_to_disk(file_path: &PathBuf, kv: &HashMap<String, String>) {
    profiling::function_scope!();

    if let Some(parent_dir) = file_path.parent() {
        if !parent_dir.exists() {
            if let Err(err) = std::fs::create_dir_all(parent_dir) {
                log::warn!("Failed to create directory {parent_dir:?}: {err}");
            }
        }
    }

    let text = {
        profiling::scope!("ron::serialize");
        match ron::ser::to_string_pretty(&kv, Default::default()) {
            Ok(t) => t,
            Err(err) => {
                log::warn!("Failed to serialize app state: {err}");
                return;
            }
        }
    };
    // never replace a good file by something that does not parse back
    if ron::de::from_str::<HashMap<String, String>>(&text).is_err() {
        log::warn!("Refusing to write an app state that does not read back");
        return;
    }
    let tmp = file_path.with_extension("ron.tmp");
    let write = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write {
        log::warn!("Failed to write {tmp:?}: {err} (the previous state is untouched)");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // previous good state -> .bak, then the atomic swap
    if file_path.exists() {
        let _ = std::fs::copy(file_path, file_path.with_extension("ron.bak"));
    }
    if let Err(err) = std::fs::rename(&tmp, file_path) {
        log::warn!("Failed to move {tmp:?} into place: {err}");
        return;
    }
    log::trace!("Persisted to {:?}", file_path);
    rotate_backups(file_path);
}

fn backup_dir(file_path: &Path) -> Option<PathBuf> {
    file_path.parent().map(|p| p.join("backups"))
}

/// One snapshot per hour (kept: the newest 72 = three days of hourly states).
fn rotate_backups(file_path: &Path) {
    let Some(dir) = backup_dir(file_path) else { return };
    let hour = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 3600).unwrap_or(0);
    let target = dir.join(format!("app-{hour:08}.ron"));
    if target.exists() || std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if std::fs::copy(file_path, &target).is_err() {
        return;
    }
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map_or(false, |x| x == "ron")).collect()).unwrap_or_default();
    names.sort();
    while names.len() > 72 {
        let _ = std::fs::remove_file(names.remove(0));
    }
}

/// Reads the state; when the file is damaged the damaged copy is set aside (`app.corrupt-<time>.ron`) and the newest readable of
/// `app.ron.bak` / `backups/*` is used instead — a damaged file is never silently replaced by an empty workspace.
fn read_with_recovery(path: &Path) -> Option<HashMap<String, String>> {
    if let Some(v) = read_ron::<HashMap<String, String>>(path) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    let exists = std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false);
    if !exists {
        // first start, or the file vanished: a leftover temp file / backup is still better than nothing
        if !path.with_extension("ron.bak").exists() && !backup_dir(path).map_or(false, |d| d.exists()) {
            return None;
        }
    } else {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = std::fs::copy(path, path.with_file_name(format!("app.corrupt-{stamp}.ron")));
        log::warn!("{path:?} is damaged — recovering from a backup");
    }
    let mut candidates = vec![path.with_extension("ron.bak")];
    if let Some(dir) = backup_dir(path) {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&dir).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
        v.sort();
        v.reverse();
        candidates.extend(v);
    }
    for c in candidates {
        if let Some(v) = read_ron::<HashMap<String, String>>(&c) {
            if !v.is_empty() {
                log::warn!("Recovered the workspace from {c:?}");
                return Some(v);
            }
        }
    }
    None
}

// ----------------------------------------------------------------------------

fn read_ron<T>(ron_path: impl AsRef<Path>) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    profiling::function_scope!();
    match std::fs::File::open(ron_path) {
        Ok(file) => {
            let reader = std::io::BufReader::new(file);
            match ron::de::from_reader(reader) {
                Ok(value) => Some(value),
                Err(err) => {
                    log::warn!("Failed to parse RON: {}", err);
                    None
                }
            }
        }
        Err(_err) => {
            // File probably doesn't exist. That's fine.
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directories_storage_dir(app_id: &str) -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", app_id)
            .map(|proj_dirs| proj_dirs.data_dir().to_path_buf())
    }

    #[test]
    fn storage_path_matches_directories() {
        use super::storage_dir;
        for app_id in [
            "MyApp", "My App", "my_app", "my-app", "My.App", "my/app", "my:app", r"my\app",
        ] {
            assert_eq!(directories_storage_dir(app_id), storage_dir(app_id));
        }
    }
}
