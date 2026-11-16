use std::collections::HashMap;
use std::path::{Path, PathBuf};

const APP_SEARCH_MAX_DEPTH: usize = 5;
const APP_SEARCH_MAX_ENTRIES: usize = 2_000;
const APP_SEARCH_MAX_RESULTS: usize = 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredFile {
    pub path: PathBuf,
    pub display_name: String,
    pub(crate) authorized_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredApp {
    pub path: PathBuf,
    pub display_name: String,
    trusted_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppTrust {
    StartMenu,
    AppPathsRegistry,
}

#[derive(Debug, Default)]
pub struct CandidateVault {
    next_id: u64,
    files: HashMap<String, RegisteredFile>,
    apps: HashMap<String, RegisteredApp>,
}

impl CandidateVault {
    pub fn register_dropped_file(
        &mut self,
        path: &Path,
    ) -> Result<(String, RegisteredFile), String> {
        let path = path
            .canonicalize()
            .map_err(|_| "拖入的文件不存在或无法访问".to_string())?;
        if !path.is_file() {
            return Err("只能拖入文件，不能拖入文件夹".to_string());
        }
        let display_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.trim().is_empty())
            .ok_or("文件名无效")?
            .to_string();
        let authorized_root = path
            .parent()
            .map(Path::to_path_buf)
            .ok_or("无法确认文件所在目录")?;
        let candidate_id = loop {
            let candidate_id = format!("drop-{:032x}", rand::random::<u128>());
            if !self.files.contains_key(&candidate_id) {
                break candidate_id;
            }
        };
        let registered = RegisteredFile {
            path,
            display_name,
            authorized_root,
        };
        self.files.insert(candidate_id.clone(), registered.clone());
        Ok((candidate_id, registered))
    }

    pub fn discard_file(&mut self, candidate_id: &str) {
        self.files.remove(candidate_id);
    }

    pub fn register_authorized_file(
        &mut self,
        authorized_roots: &[PathBuf],
        path: &Path,
        display_name: &str,
    ) -> Result<String, String> {
        let path = path
            .canonicalize()
            .map_err(|_| "文件不存在或无法访问".to_string())?;
        if !path.is_file() {
            return Err("文件不存在或无法访问".to_string());
        }
        let authorized_root = authorized_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .find(|root| path.starts_with(root))
            .ok_or("文件不在当前授权目录内")?;
        self.next_id = self.next_id.saturating_add(1);
        let candidate_id = format!("file-{}", self.next_id);
        self.files.insert(
            candidate_id.clone(),
            RegisteredFile {
                path,
                display_name: display_name.to_string(),
                authorized_root,
            },
        );
        Ok(candidate_id)
    }

    pub fn take_authorized_file(&mut self, candidate_id: &str) -> Result<RegisteredFile, String> {
        let registered = self
            .files
            .remove(candidate_id)
            .ok_or("文件候选已失效，请重新查找")?;
        let current_path = registered
            .path
            .canonicalize()
            .map_err(|_| "文件候选已失效，请重新查找".to_string())?;
        if !current_path.is_file() || !current_path.starts_with(&registered.authorized_root) {
            return Err("文件候选已失效，请重新查找".to_string());
        }
        Ok(RegisteredFile {
            path: current_path,
            ..registered
        })
    }

    pub fn file_label(&self, candidate_id: &str) -> Result<String, String> {
        self.files
            .get(candidate_id)
            .map(|registered| registered.display_name.clone())
            .ok_or("文件候选已失效，请重新查找".to_string())
    }

    pub fn snapshot_authorized_file(&self, candidate_id: &str) -> Result<RegisteredFile, String> {
        let registered = self
            .files
            .get(candidate_id)
            .ok_or("文件候选已失效，请重新查找")?;
        let current_path = registered
            .path
            .canonicalize()
            .map_err(|_| "文件候选已失效，请重新查找".to_string())?;
        let current_root = registered
            .authorized_root
            .canonicalize()
            .map_err(|_| "授权目录已失效，请重新授权".to_string())?;
        if !current_path.is_file() || !current_path.starts_with(&current_root) {
            return Err("文件候选已失效，请重新查找".to_string());
        }
        Ok(RegisteredFile {
            path: current_path,
            display_name: registered.display_name.clone(),
            authorized_root: current_root,
        })
    }

    pub fn register_installed_app(
        &mut self,
        trusted_roots: &[PathBuf],
        app: &InstalledAppMatch,
    ) -> Result<String, String> {
        let path = app
            .path
            .canonicalize()
            .map_err(|_| "软件候选已失效，请重新查找".to_string())?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !path.is_file() || !matches!(extension.as_str(), "lnk" | "exe") {
            return Err("软件候选已失效，请重新查找".to_string());
        }
        let trusted_root = match app.trust {
            AppTrust::StartMenu => trusted_roots
                .iter()
                .filter_map(|root| root.canonicalize().ok())
                .find(|root| path.starts_with(root))
                .ok_or("软件不在可信的开始菜单目录内")?,
            AppTrust::AppPathsRegistry => path
                .parent()
                .map(Path::to_path_buf)
                .ok_or("软件候选已失效，请重新查找")?,
        };
        self.next_id = self.next_id.saturating_add(1);
        let candidate_id = format!("app-{}", self.next_id);
        self.apps.insert(
            candidate_id.clone(),
            RegisteredApp {
                path,
                display_name: app.name.clone(),
                trusted_root,
            },
        );
        Ok(candidate_id)
    }

    pub fn take_installed_app(&mut self, candidate_id: &str) -> Result<RegisteredApp, String> {
        let registered = self
            .apps
            .remove(candidate_id)
            .ok_or("软件候选已失效，请重新查找")?;
        let current_path = registered
            .path
            .canonicalize()
            .map_err(|_| "软件候选已失效，请重新查找".to_string())?;
        if !current_path.is_file() || !current_path.starts_with(&registered.trusted_root) {
            return Err("软件候选已失效，请重新查找".to_string());
        }
        Ok(RegisteredApp {
            path: current_path,
            ..registered
        })
    }

    pub fn app_label(&self, candidate_id: &str) -> Result<String, String> {
        self.apps
            .get(candidate_id)
            .map(|registered| registered.display_name.clone())
            .ok_or("软件候选已失效，请重新查找".to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledAppMatch {
    pub name: String,
    pub source: &'static str,
    pub(crate) path: PathBuf,
    trust: AppTrust,
}

pub fn default_start_menu_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(app_data) = std::env::var_os("APPDATA") {
        roots.push(
            PathBuf::from(app_data)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    if let Some(program_data) = std::env::var_os("ProgramData") {
        roots.push(
            PathBuf::from(program_data)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    roots
}

fn normalize_registry_app_path(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('%') {
        return None;
    }
    let executable = if let Some(quoted) = raw.strip_prefix('"') {
        quoted.split_once('"').map(|(path, _)| path)?
    } else {
        let lower = raw.to_ascii_lowercase();
        let end = lower.find(".exe")? + 4;
        &raw[..end]
    };
    let path = PathBuf::from(executable.trim());
    let is_executable = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
    (path.is_absolute() && is_executable).then_some(path)
}

pub fn search_start_menu_roots(
    roots: &[PathBuf],
    query: &str,
) -> Result<Vec<InstalledAppMatch>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("软件名称不能为空".to_string());
    }
    if query.chars().count() > 80
        || query.contains(['/', '\\'])
        || query.chars().any(char::is_control)
    {
        return Err("软件名称不能包含路径".to_string());
    }
    let query = query.to_lowercase();
    let mut matches = Vec::new();
    let mut scanned = 0usize;
    'roots: for root in roots {
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        if !root.is_dir() {
            continue;
        }
        let mut directories = vec![(root.clone(), 0usize)];
        while let Some((directory, depth)) = directories.pop() {
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                scanned += 1;
                if scanned > APP_SEARCH_MAX_ENTRIES {
                    break 'roots;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_symlink() {
                    continue;
                }
                let path = entry.path();
                if file_type.is_dir() {
                    if depth < APP_SEARCH_MAX_DEPTH {
                        directories.push((path, depth + 1));
                    }
                    continue;
                }
                let extension = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default();
                if !matches!(extension.to_ascii_lowercase().as_str(), "lnk" | "exe") {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                if !name.to_lowercase().contains(&query) {
                    continue;
                }
                let Ok(path) = path.canonicalize() else {
                    continue;
                };
                if matches
                    .iter()
                    .any(|known: &InstalledAppMatch| known.path == path)
                {
                    continue;
                }
                matches.push(InstalledAppMatch {
                    name: name.to_string(),
                    source: "开始菜单",
                    path,
                    trust: AppTrust::StartMenu,
                });
                if matches.len() >= APP_SEARCH_MAX_RESULTS {
                    break 'roots;
                }
            }
        }
    }
    matches.sort_by(|left, right| {
        let left_exact = left.name.eq_ignore_ascii_case(&query);
        let right_exact = right.name.eq_ignore_ascii_case(&query);
        right_exact
            .cmp(&left_exact)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(matches)
}

fn normalized_app_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn same_app_identity(left: &InstalledAppMatch, right: &InstalledAppMatch, query: &str) -> bool {
    if left
        .path
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.path.to_string_lossy())
    {
        return true;
    }

    let query = normalized_app_name(query);
    if query.chars().count() < 4 || left.source == right.source {
        return false;
    }
    let left_name = normalized_app_name(&left.name);
    let right_name = normalized_app_name(&right.name);
    left_name == right_name
        || (left_name == query && right_name.ends_with(&query))
        || (right_name == query && left_name.ends_with(&query))
}

fn deduplicate_app_matches(matches: &mut Vec<InstalledAppMatch>, query: &str) {
    let mut deduplicated: Vec<InstalledAppMatch> = Vec::with_capacity(matches.len());
    for app in matches.drain(..) {
        if let Some(existing) = deduplicated
            .iter_mut()
            .find(|known| same_app_identity(known, &app, query))
        {
            if app.trust == AppTrust::StartMenu && existing.trust != AppTrust::StartMenu {
                *existing = app;
            }
        } else {
            deduplicated.push(app);
        }
    }
    *matches = deduplicated;
}

pub fn search_installed_apps(
    query: &str,
) -> Result<(Vec<PathBuf>, Vec<InstalledAppMatch>), String> {
    let roots = default_start_menu_roots();
    let mut matches = search_start_menu_roots(&roots, query)?;
    matches.extend(search_app_paths_registry(query));
    matches.sort_by(|left, right| {
        let left_exact = left.name.eq_ignore_ascii_case(query.trim());
        let right_exact = right.name.eq_ignore_ascii_case(query.trim());
        right_exact
            .cmp(&left_exact)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    deduplicate_app_matches(&mut matches, query);
    matches.truncate(APP_SEARCH_MAX_RESULTS);
    Ok((roots, matches))
}

#[cfg(windows)]
fn search_app_paths_registry(query: &str) -> Vec<InstalledAppMatch> {
    use std::os::windows::ffi::OsStringExt;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SAM_FLAGS,
    };

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    unsafe fn read_default_value(key: HKEY) -> Option<String> {
        let mut byte_len = 0u32;
        if RegQueryValueExW(key, PCWSTR::null(), None, None, None, Some(&mut byte_len))
            != ERROR_SUCCESS
            || byte_len < 2
        {
            return None;
        }
        let mut buffer = vec![0u16; byte_len as usize / 2 + 1];
        if RegQueryValueExW(
            key,
            PCWSTR::null(),
            None,
            None,
            Some(buffer.as_mut_ptr().cast::<u8>()),
            Some(&mut byte_len),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let length = buffer
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(buffer.len());
        Some(
            std::ffi::OsString::from_wide(&buffer[..length])
                .to_string_lossy()
                .to_string(),
        )
    }

    unsafe fn enumerate_root(
        root: HKEY,
        access: REG_SAM_FLAGS,
        query: &str,
        results: &mut Vec<InstalledAppMatch>,
    ) {
        let parent_path = wide(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths");
        let mut parent = HKEY::default();
        if RegOpenKeyExW(
            root,
            PCWSTR(parent_path.as_ptr()),
            None,
            access,
            &mut parent,
        ) != ERROR_SUCCESS
        {
            return;
        }
        let mut index = 0u32;
        loop {
            let mut name = vec![0u16; 512];
            let mut name_len = name.len() as u32;
            let status = RegEnumKeyExW(
                parent,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                None,
                None,
                None,
            );
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            index = index.saturating_add(1);
            if status != ERROR_SUCCESS {
                continue;
            }
            let key_name = String::from_utf16_lossy(&name[..name_len as usize]);
            let display_name = key_name
                .strip_suffix(".exe")
                .or_else(|| key_name.strip_suffix(".EXE"))
                .unwrap_or(&key_name)
                .to_string();
            if !display_name.to_lowercase().contains(query) {
                continue;
            }
            let key_name_wide = wide(&key_name);
            let mut child = HKEY::default();
            if RegOpenKeyExW(
                parent,
                PCWSTR(key_name_wide.as_ptr()),
                None,
                KEY_READ,
                &mut child,
            ) != ERROR_SUCCESS
            {
                continue;
            }
            let raw_path = read_default_value(child);
            let _ = RegCloseKey(child);
            let Some(path) = raw_path
                .as_deref()
                .and_then(normalize_registry_app_path)
                .and_then(|path| path.canonicalize().ok())
                .filter(|path| path.is_file())
            else {
                continue;
            };
            results.push(InstalledAppMatch {
                name: display_name,
                source: "App Paths",
                path,
                trust: AppTrust::AppPathsRegistry,
            });
        }
        let _ = RegCloseKey(parent);
    }

    let query = query.trim().to_lowercase();
    let mut results = Vec::new();
    unsafe {
        enumerate_root(HKEY_CURRENT_USER, KEY_READ, &query, &mut results);
        enumerate_root(
            HKEY_LOCAL_MACHINE,
            KEY_READ | KEY_WOW64_64KEY,
            &query,
            &mut results,
        );
        enumerate_root(
            HKEY_LOCAL_MACHINE,
            KEY_READ | KEY_WOW64_32KEY,
            &query,
            &mut results,
        );
    }
    results
}

#[cfg(not(windows))]
fn search_app_paths_registry(_query: &str) -> Vec<InstalledAppMatch> {
    Vec::new()
}

pub fn open_authorized_file_with<F>(
    vault: &mut CandidateVault,
    candidate_id: &str,
    opener: F,
) -> Result<String, String>
where
    F: FnOnce(&Path) -> Result<(), String>,
{
    let registered = vault.take_authorized_file(candidate_id)?;
    opener(&registered.path)?;
    Ok(registered.display_name)
}

pub fn open_installed_app_with<F>(
    vault: &mut CandidateVault,
    candidate_id: &str,
    opener: F,
) -> Result<String, String>
where
    F: FnOnce(&Path) -> Result<(), String>,
{
    let registered = vault.take_installed_app(candidate_id)?;
    opener(&registered.path)?;
    Ok(registered.display_name)
}

pub fn open_authorized_file(
    vault: &mut CandidateVault,
    candidate_id: &str,
) -> Result<String, String> {
    open_authorized_file_with(vault, candidate_id, shell_open)
}

pub fn open_installed_app(
    vault: &mut CandidateVault,
    candidate_id: &str,
) -> Result<String, String> {
    open_installed_app_with(vault, candidate_id, shell_open)
}

#[cfg(windows)]
fn shell_open(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(wide_path.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|error| format!("Windows 未能打开目标：{error}"))
}

#[cfg(not(windows))]
fn shell_open(_path: &Path) -> Result<(), String> {
    Err("当前系统不支持 Windows 桌面打开功能".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        deduplicate_app_matches, normalize_registry_app_path, open_authorized_file_with,
        search_start_menu_roots, AppTrust, CandidateVault, InstalledAppMatch,
    };

    fn test_directory(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cyral-windows-actions-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn file_candidate_must_be_registered_inside_an_authorized_root() {
        let authorized = test_directory("authorized");
        let outside = test_directory("outside");
        std::fs::create_dir_all(&authorized).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let allowed_file = authorized.join("课程作业.pptx");
        let denied_file = outside.join("秘密.txt");
        std::fs::write(&allowed_file, b"private").unwrap();
        std::fs::write(&denied_file, b"private").unwrap();

        let mut vault = CandidateVault::default();
        let candidate_id = vault
            .register_authorized_file(
                std::slice::from_ref(&authorized),
                &allowed_file,
                "课程作业.pptx",
            )
            .expect("授权目录内的文件应能注册为临时候选");
        assert!(!candidate_id.contains("课程作业"));
        assert_eq!(
            vault
                .take_authorized_file(&candidate_id)
                .expect("已注册候选应能解析")
                .path,
            allowed_file.canonicalize().unwrap()
        );

        let reused = vault
            .take_authorized_file(&candidate_id)
            .expect_err("候选 ID 必须是一次性的");
        assert_eq!(reused, "文件候选已失效，请重新查找");
        let denied = vault
            .register_authorized_file(std::slice::from_ref(&authorized), &denied_file, "秘密.txt")
            .expect_err("授权目录外的路径不得注册");
        assert_eq!(denied, "文件不在当前授权目录内");

        std::fs::remove_dir_all(authorized).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn start_menu_search_only_indexes_shortcuts_and_executables() {
        let root = test_directory("start-menu");
        std::fs::create_dir_all(root.join("浏览器")).unwrap();
        std::fs::write(root.join("浏览器").join("Google Chrome.lnk"), b"shortcut").unwrap();
        std::fs::write(root.join("浏览器").join("Chrome notes.txt"), b"not an app").unwrap();
        std::fs::write(root.join("Chromium.exe"), b"fake executable").unwrap();

        let matches = search_start_menu_roots(std::slice::from_ref(&root), "chrome").unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "Google Chrome");
        assert_eq!(matches[0].source, "开始菜单");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn start_menu_search_rejects_path_queries() {
        let error = search_start_menu_roots(&[], "C:\\Windows\\notepad.exe")
            .expect_err("模型不得把路径伪装成软件名");
        assert_eq!(error, "软件名称不能包含路径");
    }

    #[test]
    fn same_app_from_start_menu_and_app_paths_is_returned_once() {
        let mut matches = vec![
            InstalledAppMatch {
                name: "Google Chrome".into(),
                source: "开始菜单",
                path: std::path::PathBuf::from(
                    r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Google Chrome.lnk",
                ),
                trust: AppTrust::StartMenu,
            },
            InstalledAppMatch {
                name: "chrome".into(),
                source: "App Paths",
                path: std::path::PathBuf::from(
                    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                ),
                trust: AppTrust::AppPathsRegistry,
            },
        ];

        deduplicate_app_matches(&mut matches, "chrome");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "Google Chrome");
        assert_eq!(matches[0].source, "开始菜单");
    }

    #[test]
    fn app_candidate_must_come_from_a_trusted_start_menu_root() {
        let trusted = test_directory("trusted-apps");
        let outside = test_directory("outside-apps");
        std::fs::create_dir_all(&trusted).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let trusted_shortcut = trusted.join("Google Chrome.lnk");
        let outside_executable = outside.join("unknown.exe");
        std::fs::write(&trusted_shortcut, b"shortcut").unwrap();
        std::fs::write(&outside_executable, b"executable").unwrap();

        let mut vault = CandidateVault::default();
        let candidate_id = vault
            .register_installed_app(
                std::slice::from_ref(&trusted),
                &InstalledAppMatch {
                    name: "Google Chrome".into(),
                    source: "开始菜单",
                    path: trusted_shortcut.clone(),
                    trust: super::AppTrust::StartMenu,
                },
            )
            .expect("可信开始菜单中的快捷方式应能注册");
        assert_eq!(
            vault
                .take_installed_app(&candidate_id)
                .expect("已注册软件候选应能解析")
                .path,
            trusted_shortcut.canonicalize().unwrap()
        );

        let denied = vault
            .register_installed_app(
                std::slice::from_ref(&trusted),
                &InstalledAppMatch {
                    name: "unknown".into(),
                    source: "开始菜单",
                    path: outside_executable,
                    trust: super::AppTrust::StartMenu,
                },
            )
            .expect_err("开始菜单之外的程序不得注册");
        assert_eq!(denied, "软件不在可信的开始菜单目录内");

        std::fs::remove_dir_all(trusted).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn file_is_revalidated_before_the_platform_opener_runs() {
        let root = test_directory("opener");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("个人简历.pdf");
        std::fs::write(&path, b"private").unwrap();
        let mut vault = CandidateVault::default();
        let candidate_id = vault
            .register_authorized_file(std::slice::from_ref(&root), &path, "个人简历.pdf")
            .unwrap();
        let expected = path.canonicalize().unwrap();
        let mut opened = None;

        let result = open_authorized_file_with(&mut vault, &candidate_id, |validated| {
            opened = Some(validated.to_path_buf());
            Ok(())
        })
        .unwrap();

        assert_eq!(opened, Some(expected));
        assert_eq!(result, "个人简历.pdf");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dropped_file_registration_grants_only_that_file_and_can_be_discarded() {
        let root = test_directory("drop-file");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("课程作业.pptx");
        std::fs::write(&path, b"presentation").unwrap();
        let mut vault = CandidateVault::default();

        let (candidate_id, registered) = vault.register_dropped_file(&path).unwrap();
        assert_eq!(registered.display_name, "课程作业.pptx");
        assert_eq!(
            vault.snapshot_authorized_file(&candidate_id).unwrap().path,
            path.canonicalize().unwrap()
        );

        vault.discard_file(&candidate_id);
        assert!(vault.snapshot_authorized_file(&candidate_id).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registry_app_path_keeps_only_an_absolute_executable_path() {
        assert_eq!(
            normalize_registry_app_path(
                r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --profile-directory=Default"#,
            ),
            Some(std::path::PathBuf::from(
                r#"C:\Program Files\Google\Chrome\Application\chrome.exe"#,
            ))
        );
        assert_eq!(normalize_registry_app_path(r#"%TEMP%\unknown.exe"#), None);
        assert_eq!(normalize_registry_app_path(r#"C:\Tools\script.cmd"#), None);
        assert_eq!(normalize_registry_app_path("relative.exe"), None);
    }
}
