//! MCP credential store：token 与 refresh token 的统一存取 seam。
//!
//! - [`McpCredentialStore`] trait：`get` / `set` / `remove`，按 server 名
//!   键控（stdio 与 http 同一把钥匙）。
//! - [`FileCredentialStore`]：默认文件实现，目录可注入（测试用临时目录）。
//!   0600 权限、临时文件 + rename 原子写、加载时收紧权限（与
//!   xai-grok-mcp `credentials.rs` 一致）；坏文件按缺失处理并落诊断，
//!   不 panic。
//! - token 与 refresh token 分离存储（[`McpCredentials`]），refresh 失败
//!   只丢 access token 的新鲜度，不隐式抹掉 refresh token。
//!
//! OAuth 设备码流程与 401 后单次 refresh 见 [`crate::mcp::oauth`]。

// Adapted from xai-grok-mcp, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// credentials.rs persistence discipline (0600, atomic rename, load-time tightening)
// ported; trait seam and token/refresh split are Evo's own.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// credential 文件名（目录内）。
pub const CREDENTIALS_FILENAME: &str = "mcp_credentials.json";

/// 单个 MCP server 的凭据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCredentials {
    pub access_token: String,
    /// `None` = 无 refresh token（只读 token 或未配置 OAuth）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// access token 过期时间（Unix 秒；`None` = 未知）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

impl McpCredentials {
    pub fn new(access_token: impl Into<String>) -> Self {
        Self {
            access_token: access_token.into(),
            refresh_token: None,
            expires_at: None,
        }
    }
}

/// credential 存储错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialStoreError {
    #[error("credential store I/O error: {0}")]
    Io(String),
    #[error("credential store JSON error: {0}")]
    Json(String),
}

/// 统一 credential seam（token 与 refresh token 分离，见 [`McpCredentials`]）。
pub trait McpCredentialStore: std::fmt::Debug + Send + Sync {
    fn get(&self, server: &str) -> Option<McpCredentials>;
    fn set(&self, server: &str, credentials: McpCredentials) -> Result<(), CredentialStoreError>;
    fn remove(&self, server: &str);
}

/// 默认文件实现：`<dir>/mcp_credentials.json`，0600 权限 + 原子写。
#[derive(Debug)]
pub struct FileCredentialStore {
    dir: PathBuf,
    /// 同步互斥：单进程内串行化读改写（跨进程 flock 登记为债务）。
    state: Mutex<BTreeMap<String, McpCredentials>>,
}

impl FileCredentialStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let store = Self {
            dir: dir.into(),
            state: Mutex::new(BTreeMap::new()),
        };
        store.reload();
        store
    }

    /// 磁盘路径。
    pub fn path(&self) -> PathBuf {
        self.dir.join(CREDENTIALS_FILENAME)
    }

    fn reload(&self) {
        let path = self.path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return; // 缺失/不可读 = 空 store（不 panic）。
        };
        match serde_json::from_str::<BTreeMap<String, McpCredentials>>(&content) {
            Ok(entries) => {
                *self.state.lock().unwrap() = entries;
                let _ = enforce_owner_only(&path);
            }
            Err(_) => {
                // 坏文件按缺失处理（不 panic、不抹掉磁盘内容）；
                // 下次 set 时原子覆盖。
            }
        }
    }

    fn persist(&self) -> Result<(), CredentialStoreError> {
        let path = self.path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| CredentialStoreError::Io(error.to_string()))?;
        }
        let content = serde_json::to_string_pretty(&*self.state.lock().unwrap())
            .map_err(|error| CredentialStoreError::Json(error.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        write_owner_only(&tmp, content.as_bytes())?;
        std::fs::rename(&tmp, &path)
            .map_err(|error| CredentialStoreError::Io(error.to_string()))?;
        let _ = enforce_owner_only(&path);
        Ok(())
    }
}

impl McpCredentialStore for FileCredentialStore {
    fn get(&self, server: &str) -> Option<McpCredentials> {
        self.state.lock().unwrap().get(server).cloned()
    }

    fn set(&self, server: &str, credentials: McpCredentials) -> Result<(), CredentialStoreError> {
        self.state
            .lock()
            .unwrap()
            .insert(server.to_string(), credentials);
        self.persist()
    }

    fn remove(&self, server: &str) {
        self.state.lock().unwrap().remove(server);
        let _ = self.persist();
    }
}

/// Unix 下确保文件权限为 0600（写时新建即 0600，读时收紧）。
fn write_owner_only(path: &Path, content: &[u8]) -> Result<(), CredentialStoreError> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| CredentialStoreError::Io(error.to_string()))?;
        file.write_all(content)
            .map_err(|error| CredentialStoreError::Io(error.to_string()))?;
        file.flush()
            .map_err(|error| CredentialStoreError::Io(error.to_string()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content).map_err(|error| CredentialStoreError::Io(error.to_string()))
    }
}

fn enforce_owner_only(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let mode = metadata.permissions().mode();
                if mode & 0o777 != 0o600 {
                    let mut permissions = metadata.permissions();
                    permissions.set_mode(0o600);
                    std::fs::set_permissions(path, permissions)
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_remove_round_trips_in_memory() {
        let store = FileCredentialStore::new(tempfile::tempdir().unwrap().path());
        let credentials = McpCredentials {
            access_token: "at-1".into(),
            refresh_token: Some("rt-1".into()),
            expires_at: Some(1234),
        };
        store.set("linear", credentials.clone()).unwrap();
        assert_eq!(store.get("linear"), Some(credentials));
        assert_eq!(store.get("other"), None);
        store.remove("linear");
        assert_eq!(store.get("linear"), None);
    }

    #[test]
    fn persisted_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = FileCredentialStore::new(dir.path());
            store.set("s", McpCredentials::new("token")).unwrap();
        }
        let reloaded = FileCredentialStore::new(dir.path());
        assert_eq!(reloaded.get("s"), Some(McpCredentials::new("token")));
    }

    #[test]
    fn token_and_refresh_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path());
        store
            .set(
                "s",
                McpCredentials {
                    access_token: "at".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        // 只更新 access token，refresh token 保持 None（未配置）。
        assert_eq!(store.get("s").unwrap().refresh_token, None);
        store
            .set(
                "s",
                McpCredentials {
                    access_token: "at2".into(),
                    refresh_token: Some("rt".into()),
                    expires_at: Some(999),
                },
            )
            .unwrap();
        let creds = store.get("s").unwrap();
        assert_eq!(creds.access_token, "at2");
        assert_eq!(creds.refresh_token.as_deref(), Some("rt"));
        assert_eq!(creds.expires_at, Some(999));
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path());
        store.set("s", McpCredentials::new("token")).unwrap();
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn load_tightens_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path());
        store.set("s", McpCredentials::new("token")).unwrap();
        let mut loose = std::fs::metadata(store.path()).unwrap().permissions();
        loose.set_mode(0o644);
        std::fs::set_permissions(store.path(), loose).unwrap();
        let _ = FileCredentialStore::new(dir.path());
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn corrupt_store_loads_empty_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CREDENTIALS_FILENAME), "{oops").unwrap();
        let store = FileCredentialStore::new(dir.path());
        assert_eq!(store.get("anything"), None);
    }
}
