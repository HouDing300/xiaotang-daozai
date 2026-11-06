//! 一键抓取 WorkBuddy 官网登录态。
//!
//! 不碰用户日常浏览器的配置：用一个**专属 user-data-dir** 打开 Edge 窗口，
//! 用户在里面登录后关闭窗口，岛仔直接从那个配置目录里读出会话 Cookie。
//!
//! 为什么不能读日常浏览器：Chrome/Edge 127+ 对默认配置启用了 app-bound 加密，
//! 密钥需要提权才能取出；而新建配置目录仍使用当前用户的 DPAPI 主密钥（v10），
//! 岛仔本来就有 DPAPI 调用，可以自己解开。这样既不读取用户日常 Cookie，
//! 也不需要 F12 手抄请求头。

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// 只保留官网登录需要的 Cookie，和 `workbuddy_quota` 里的白名单保持一致。
const COOKIE_NAMES: [&str; 4] = ["session", "session_2", "cb_login_mode", "cb_login_realm"];

pub struct BrowserLogin {
    child: Option<Child>,
    profile: PathBuf,
    started: i64,
    last_try: i64,
    finished: bool,
}

fn edge_path() -> Option<PathBuf> {
    let mut candidates = vec![
        PathBuf::from(r"C://Program Files (x86)//Microsoft//Edge//Application//msedge.exe"),
        PathBuf::from(r"C://Program Files//Microsoft//Edge//Application//msedge.exe"),
    ];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join(r"Microsoft\Edge\Application\msedge.exe"));
    }
    candidates.into_iter().find(|path| path.exists())
}

fn profile_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Daozai")
        .join("login-profile")
}

impl BrowserLogin {
    /// 打开专属登录窗口；已有实例时先结束旧的。
    pub fn start() -> Result<Self, String> {
        let edge = edge_path().ok_or_else(|| "没有找到 Microsoft Edge，请先安装，或改用下面的 Cookie 方式连接".to_string())?;
        let profile = profile_dir();
        std::fs::create_dir_all(&profile).map_err(|e| format!("无法创建登录配置目录：{e}"))?;
        let child = Command::new(edge)
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-features=msEdgeSidebarV2")
            .arg("--app=https://www.codebuddy.cn/profile/usage")
            .arg("--window-size=1000,760")
            .spawn()
            .map_err(|e| format!("无法启动 Edge：{e}"))?;
        Ok(Self {
            child: Some(child),
            profile,
            started: chrono::Local::now().timestamp(),
            last_try: 0,
            finished: false,
        })
    }

    /// 关闭登录窗口（用户中途取消时调用）。
    pub fn cancel(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Edge 可能还有子进程，按进程树结束，避免留下后台窗口。
            let _ = Command::new("taskkill")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let _ = child.wait();
        }
        self.finished = true;
    }

    /// 每两秒试读一次：数据库能读了说明窗口已经关掉，登录态也就落盘了。
    pub fn poll(&mut self, now: i64) -> Option<Result<String, String>> {
        if self.finished {
            return None;
        }
        if now - self.last_try < 2 {
            return None;
        }
        self.last_try = now;
        match read_cookie_header(&self.profile) {
            Ok(header) => {
                self.finished = true;
                Some(Ok(header))
            }
            Err(error) if error.contains("还没有登录记录") || error.contains("还在占用") || error.contains("请稍等") => {
                // 窗口可能还开着，或者刚启动还没写盘；最多等 5 分钟。
                if now - self.started > 300 {
                    self.finished = true;
                    Some(Err("等待登录超时：请重新点「自动获取登录态」，或改用手动粘贴 Cookie".into()))
                } else {
                    None
                }
            }
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}

impl Drop for BrowserLogin {
    fn drop(&mut self) {
        // 正常流程里窗口已经关了；取消时确保不留下后台进程。
        if let Some(mut child) = self.child.take() {
            let _ = child.try_wait();
            let _ = child.kill();
        }
    }
}

/// 读取专属配置目录里的官网 Cookie，拼成可以直接用的请求头。
pub(crate) fn read_cookie_header(profile: &Path) -> Result<String, String> {
    let cookies = profile.join("Default").join("Network").join("Cookies");
    if !cookies.exists() {
        return Err("还没有登录记录：请在弹出的窗口里完成 WorkBuddy 官网登录，然后关闭窗口".into());
    }
    let key = master_key(profile)?;
    // 数据库可能被浏览器占用，复制一份再查。
    let temp = std::env::temp_dir().join(format!("daozai-cookies-{}.db", std::process::id()));
    std::fs::copy(&cookies, &temp).map_err(|_| "浏览器还在占用登录数据，请先关闭那个登录窗口".to_string())?;
    let rows = query_cookies(&temp);
    let _ = std::fs::remove_file(&temp);
    let rows = rows?;
    let mut kept: Vec<String> = Vec::new();
    for (host, name, encrypted) in rows {
        if !(host.contains("workbuddy.cn") || host.contains("codebuddy.cn")) {
            continue;
        }
        if !COOKIE_NAMES.contains(&name.as_str()) {
            continue;
        }
        let Ok(value) = decrypt_cookie(&key, &encrypted) else { continue };
        let value = String::from_utf8_lossy(&value).to_string();
        if value.is_empty() || value.contains([';', '\r', '\n']) {
            continue;
        }
        kept.push(format!("{name}={value}"));
    }
    if !kept.iter().any(|s| s.starts_with("session=") || s.starts_with("session_2=")) {
        return Err("窗口里还没登录 WorkBuddy，或登录没有完成。请重新打开并登录后再关闭。".into());
    }
    kept.sort();
    kept.dedup();
    Ok(kept.join("; "))
}

fn master_key(profile: &Path) -> Result<Vec<u8>, String> {
    let raw = std::fs::read_to_string(profile.join("Local State")).map_err(|_| "登录配置还没生成，请稍等再试".to_string())?;
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|_| "登录配置损坏，请重新打开登录窗口".to_string())?;
    let key = value
        .pointer("/os_crypt/encrypted_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "登录配置里没有加密密钥".to_string())?;
    let bytes = base64_decode(key).ok_or_else(|| "登录配置的密钥格式无效".to_string())?;
    let blob = bytes.strip_prefix(b"DPAPI").ok_or_else(|| "登录配置的密钥类型不支持".to_string())?;
    unprotect(blob)
}

/// AES-256-GCM 解密（v10 前缀）。
fn decrypt_cookie(key: &[u8], blob: &[u8]) -> Result<Vec<u8>, String> {
    if let Some(body) = blob.strip_prefix(b"v10") {
        if body.len() < 12 + 16 || key.len() != 32 {
            return Err("Cookie 数据损坏".into());
        }
        let (nonce, rest) = body.split_at(12);
        let (cipher, tag) = rest.split_at(rest.len() - 16);
        return aes_gcm(key, nonce, cipher, tag);
    }
    if blob.starts_with(b"v20") {
        return Err("这个浏览器启用了新版加密，无法读取".into());
    }
    unprotect(blob)
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
            .map_err(|_| "无法用当前 Windows 账户解密登录数据".to_string())?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut core::ffi::c_void));
        Ok(bytes)
    }
}

#[cfg(not(windows))]
fn unprotect(_data: &[u8]) -> Result<Vec<u8>, String> {
    Err("仅支持 Windows".into())
}

#[cfg(windows)]
fn aes_gcm(key: &[u8], nonce: &[u8], cipher: &[u8], tag: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Security::Cryptography::{
        BCryptCloseAlgorithmProvider, BCryptDecrypt, BCryptDestroyKey, BCryptGenerateSymmetricKey,
        BCryptOpenAlgorithmProvider, BCryptSetProperty, BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO,
        BCRYPT_AES_ALGORITHM, BCRYPT_CHAINING_MODE, BCRYPT_CHAIN_MODE_GCM, BCRYPT_HANDLE,
        BCRYPT_KEY_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS,
    };
    unsafe {
        let mut algorithm = Default::default();
        BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_AES_ALGORITHM, None, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0))
            .ok()
            .map_err(|_| "无法初始化解密算法".to_string())?;
        let mode = BCRYPT_CHAIN_MODE_GCM.as_wide();
        let mode_bytes = core::slice::from_raw_parts(mode.as_ptr() as *const u8, mode.len() * 2);
        BCryptSetProperty(BCRYPT_HANDLE(algorithm.0), BCRYPT_CHAINING_MODE, mode_bytes, 0)
            .ok()
            .map_err(|_| "无法设置解密模式".to_string())?;
        let mut key_handle = BCRYPT_KEY_HANDLE::default();
        BCryptGenerateSymmetricKey(algorithm, &mut key_handle, None, key, 0)
            .ok()
            .map_err(|_| "无法导入解密密钥".to_string())?;
        let mut nonce_buf = nonce.to_vec();
        let mut tag_buf = tag.to_vec();
        let mut info = BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO::default();
        info.pbNonce = nonce_buf.as_mut_ptr();
        info.cbNonce = nonce_buf.len() as u32;
        info.pbTag = tag_buf.as_mut_ptr();
        info.cbTag = tag_buf.len() as u32;
        info.dwInfoVersion = 1;
        info.cbSize = std::mem::size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32;
        let mut out = vec![0u8; cipher.len().max(1)];
        let mut written = 0u32;
        let status = BCryptDecrypt(
            key_handle,
            Some(cipher),
            Some(&info as *const _ as *const core::ffi::c_void),
            None,
            Some(&mut out),
            &mut written,
            Default::default(),
        );
        let _ = BCryptDestroyKey(key_handle);
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
        status.ok().map_err(|_| "Cookie 解密失败".to_string())?;
        out.truncate(written as usize);
        Ok(out)
    }
}

#[cfg(not(windows))]
fn aes_gcm(_key: &[u8], _nonce: &[u8], _cipher: &[u8], _tag: &[u8]) -> Result<Vec<u8>, String> {
    Err("仅支持 Windows".into())
}

#[cfg(windows)]
fn query_cookies(db: &Path) -> Result<Vec<(String, String, Vec<u8>)>, String> {
    use std::ffi::{c_char, c_int, c_uchar, c_void, CString};
    #[link(name = "winsqlite3")]
    extern "system" {
        fn sqlite3_open_v2(filename: *const c_char, db: *mut *mut c_void, flags: c_int, vfs: *const c_char) -> c_int;
        fn sqlite3_prepare_v2(db: *mut c_void, sql: *const c_char, n: c_int, stmt: *mut *mut c_void, tail: *mut *const c_char) -> c_int;
        fn sqlite3_step(stmt: *mut c_void) -> c_int;
        fn sqlite3_column_text(stmt: *mut c_void, i: c_int) -> *const c_uchar;
        fn sqlite3_column_blob(stmt: *mut c_void, i: c_int) -> *const c_void;
        fn sqlite3_column_bytes(stmt: *mut c_void, i: c_int) -> c_int;
        fn sqlite3_finalize(stmt: *mut c_void) -> c_int;
        fn sqlite3_close(db: *mut c_void) -> c_int;
    }
    const ROW: c_int = 100;
    let path = CString::new(db.to_string_lossy().as_bytes()).map_err(|_| "路径无效".to_string())?;
    let sql = CString::new("SELECT host_key,name,encrypted_value FROM cookies").unwrap();
    unsafe {
        let mut handle: *mut c_void = std::ptr::null_mut();
        if sqlite3_open_v2(path.as_ptr(), &mut handle, 1, std::ptr::null()) != 0 {
            if !handle.is_null() {
                sqlite3_close(handle);
            }
            return Err("无法打开浏览器 Cookie 数据库".into());
        }
        let mut stmt: *mut c_void = std::ptr::null_mut();
        if sqlite3_prepare_v2(handle, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) != 0 {
            sqlite3_close(handle);
            return Err("浏览器 Cookie 数据库结构不支持".into());
        }
        let mut rows = Vec::new();
        while sqlite3_step(stmt) == ROW {
            let text = |i: c_int| -> String {
                let ptr = sqlite3_column_text(stmt, i);
                if ptr.is_null() {
                    return String::new();
                }
                let len = sqlite3_column_bytes(stmt, i).max(0) as usize;
                String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).to_string()
            };
            let host = text(0);
            let name = text(1);
            let blob_ptr = sqlite3_column_blob(stmt, 2);
            let blob_len = sqlite3_column_bytes(stmt, 2).max(0) as usize;
            let blob = if blob_ptr.is_null() || blob_len == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(blob_ptr as *const u8, blob_len).to_vec()
            };
            rows.push((host, name, blob));
        }
        sqlite3_finalize(stmt);
        sqlite3_close(handle);
        Ok(rows)
    }
}

#[cfg(not(windows))]
fn query_cookies(_db: &Path) -> Result<Vec<(String, String, Vec<u8>)>, String> {
    Err("仅支持 Windows".into())
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for byte in input.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let digit = value(byte)?;
        buffer = (buffer << 6) | digit as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip_accepts_standard_and_urlsafe() {
        assert_eq!(base64_decode("RFBBUEk=").unwrap(), b"DPAPI".to_vec());
        assert_eq!(base64_decode("RFBBUEk").unwrap(), b"DPAPI".to_vec());
        assert!(base64_decode("!!!").is_none());
    }

    /// 用 `DAOZAI_COOKIE_PROBE` 指向一个浏览器配置目录时，实跑一次读取链路。
    /// 没有设置该环境变量时自动跳过，CI 上不会依赖真实浏览器数据。
    #[test]
    fn reads_profile_when_env_points_to_it() {
        let Ok(path) = std::env::var("DAOZAI_COOKIE_PROBE") else { return };
        let header = read_cookie_header(Path::new(&path)).expect("应能读出测试 Cookie");
        assert!(header.contains("session=TESTVALUE"), "{header}");
        assert!(!header.contains("other-site"), "不应带出其它站点的 Cookie：{header}");
    }

    #[test]
    fn unsupported_prefixes_are_rejected() {
        assert!(decrypt_cookie(&[0u8; 32], b"v20-anything").is_err());
        assert!(decrypt_cookie(&[0u8; 32], b"v10short").is_err());
    }
}
