// SPDX-License-Identifier: Apache-2.0
//! Login sessions of `authorizationCode` schemes: where their tokens live,
//! which account a command uses, and the index of what is stored.
//!
//! A session is keyed by its partition: scheme, account, token endpoint
//! (the issuer), client id, the origin of the base URL and the requested
//! scopes. A change of any of them selects a different session, so one
//! identity's tokens are never sent for another account, client or
//! deployment.
//!
//! Identity and provenance (the partition, the store, creation time,
//! expiry and whether a refresh token exists) are kept in a non-secret
//! index, `sessions.json` next to the default config file. Token values are
//! kept only by the selected store:
//!
//! - `keychain` (the default): the macOS login keychain through
//!   `/usr/bin/security`, or the Secret Service (GNOME Keyring, KWallet)
//!   through `secret-tool` on Linux and the BSDs. Other platforms have no
//!   keychain adapter; the store is reported as unavailable.
//! - `file`: one file per session, created with mode 0600, under
//!   `sessions/` next to the index. Only used when chosen explicitly
//!   (`--store file`, `<PREFIX>_SESSION_STORE=file` or the profile key
//!   `session_store = "file"`); an unavailable or locked keychain never falls
//!   back to it.
//! - `ephemeral`: nothing is read or written; commands use only the
//!   credentials of the environment and profile.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tungsten_runtime::internals::sha256_hex;
use tungsten_runtime::{AuthSchemeDescriptor, StoredToken, TokenStore};

use crate::config::{self, Settings, Source};
use crate::spec::CliSpec;

/// The account used when none is named.
pub(crate) const DEFAULT_ACCOUNT: &str = "default";

/// Where session tokens are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreKind {
    Keychain,
    File,
    Ephemeral,
}

impl StoreKind {
    pub(crate) fn parse(text: &str, from: &str) -> Result<StoreKind, String> {
        match text {
            "keychain" => Ok(StoreKind::Keychain),
            "file" => Ok(StoreKind::File),
            "ephemeral" => Ok(StoreKind::Ephemeral),
            _ => Err(format!(
                "{from}: the session store must be `keychain`, `file` or `ephemeral`"
            )),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            StoreKind::Keychain => "keychain",
            StoreKind::File => "file",
            StoreKind::Ephemeral => "ephemeral",
        }
    }
}

/// What identifies one stored session. Nothing in it is secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Partition {
    pub scheme: String,
    pub account: String,
    /// The token endpoint of the scheme's `authorizationCode` flow.
    pub issuer: String,
    pub client_id: String,
    /// `scheme://host[:port]` of the base URL.
    pub origin: String,
    /// Requested scopes, sorted.
    pub scopes: Vec<String>,
}

impl Partition {
    /// The storage key: a digest of every member.
    pub(crate) fn id(&self) -> String {
        let doc = json!([
            self.scheme,
            self.account,
            self.issuer,
            self.client_id,
            self.origin,
            self.scopes
        ]);
        let hex = sha256_hex(doc.to_string().as_bytes());
        hex[..32].to_string()
    }

    /// Same session apart from the requested scopes.
    fn same_principal(&self, other: &Partition) -> bool {
        self.scheme == other.scheme
            && self.account == other.account
            && self.issuer == other.issuer
            && self.client_id == other.client_id
            && self.origin == other.origin
    }
}

/// `scheme://host[:port]` of `url`, lower-cased; the URL itself when it does
/// not parse.
pub(crate) fn origin(url: &str) -> String {
    let lower = url.trim().to_ascii_lowercase();
    let Some((scheme, rest)) = lower.split_once("://") else {
        return lower;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or("");
    let default_port = match scheme {
        "https" => Some(":443"),
        "http" => Some(":80"),
        _ => None,
    };
    let host = match default_port {
        Some(p) if authority.ends_with(p) => &authority[..authority.len() - p.len()],
        _ => authority,
    };
    format!("{scheme}://{host}")
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// One entry of the index.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub partition: Partition,
    pub store: StoreKind,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub refreshable: bool,
    /// Granted scopes as the token endpoint reported them.
    pub granted_scope: Option<String>,
}

impl Entry {
    fn to_json(&self) -> Value {
        let p = &self.partition;
        json!({
            "id": p.id(),
            "scheme": p.scheme,
            "account": p.account,
            "issuer": p.issuer,
            "client_id": p.client_id,
            "origin": p.origin,
            "scopes": p.scopes,
            "store": self.store.name(),
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "refreshable": self.refreshable,
            "granted_scope": self.granted_scope,
        })
    }

    fn from_json(v: &Value) -> Option<Entry> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(Entry {
            partition: Partition {
                scheme: s("scheme")?,
                account: s("account")?,
                issuer: s("issuer")?,
                client_id: s("client_id")?,
                origin: s("origin")?,
                scopes: v
                    .get("scopes")?
                    .as_array()?
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect(),
            },
            store: StoreKind::parse(v.get("store")?.as_str()?, "index").ok()?,
            created_at: v.get("created_at").and_then(Value::as_u64).unwrap_or(0),
            expires_at: v.get("expires_at").and_then(Value::as_u64),
            refreshable: v
                .get("refreshable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            granted_scope: s("granted_scope"),
        })
    }

    /// The non-secret status of the entry.
    pub(crate) fn status_json(&self, selected: bool, now: u64) -> Value {
        let mut v = self.to_json();
        if let Value::Object(map) = &mut v {
            map.insert("selected".into(), json!(selected));
            map.insert(
                "expired".into(),
                json!(self.expires_at.is_some_and(|e| e <= now)),
            );
        }
        v
    }
}

/// The non-secret index of stored sessions and the account each profile
/// selected per scheme.
#[derive(Debug, Default, Clone)]
pub(crate) struct Index {
    pub entries: Vec<Entry>,
    /// `<profile>\n<scheme>` to account.
    pub active: BTreeMap<String, String>,
}

fn active_key(profile: &str, scheme: &str) -> String {
    format!("{profile}\n{scheme}")
}

impl Index {
    pub(crate) fn load(dir: &Path) -> Result<Index, String> {
        let path = dir.join("sessions.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Index::default()),
            Err(e) => return Err(format!("session index {}: {e}", path.display())),
        };
        let doc: Value = serde_json::from_str(&text)
            .map_err(|e| format!("session index {}: {e}", path.display()))?;
        let entries = doc
            .get("sessions")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Entry::from_json).collect())
            .unwrap_or_default();
        let mut active = BTreeMap::new();
        if let Some(list) = doc.get("active").and_then(Value::as_array) {
            for a in list {
                let s = |k: &str| a.get(k).and_then(Value::as_str);
                if let (Some(p), Some(sc), Some(acc)) = (s("profile"), s("scheme"), s("account")) {
                    active.insert(active_key(p, sc), acc.to_string());
                }
            }
        }
        Ok(Index { entries, active })
    }

    pub(crate) fn save(&self, dir: &Path) -> Result<(), String> {
        let active: Vec<Value> = self
            .active
            .iter()
            .filter_map(|(k, acc)| {
                let (p, sc) = k.split_once('\n')?;
                Some(json!({ "profile": p, "scheme": sc, "account": acc }))
            })
            .collect();
        let doc = json!({
            "version": 1,
            "sessions": self.entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
            "active": active,
        });
        let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
        write_private(&dir.join("sessions.json"), text.as_bytes())
            .map_err(|e| format!("session index in {}: {e}", dir.display()))
    }

    pub(crate) fn active_account(&self, profile: &str, scheme: &str) -> Option<&str> {
        self.active
            .get(&active_key(profile, scheme))
            .map(String::as_str)
    }

    pub(crate) fn set_active(&mut self, profile: &str, scheme: &str, account: &str) {
        self.active
            .insert(active_key(profile, scheme), account.to_string());
    }

    pub(crate) fn clear_active(&mut self, scheme: &str, account: &str) {
        self.active
            .retain(|k, a| !(k.ends_with(&format!("\n{scheme}")) && a == account));
    }

    /// The stored session of `partition`: the exact one, or one of the same
    /// principal whose requested scopes include every scope asked for.
    pub(crate) fn find(&self, partition: &Partition) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.partition == *partition)
            .or_else(|| {
                self.entries.iter().find(|e| {
                    e.partition.same_principal(partition)
                        && partition
                            .scopes
                            .iter()
                            .all(|s| e.partition.scopes.contains(s))
                })
            })
    }

    pub(crate) fn upsert(&mut self, entry: Entry) {
        self.entries.retain(|e| e.partition != entry.partition);
        self.entries.push(entry);
    }

    pub(crate) fn remove(&mut self, partition: &Partition) {
        self.entries.retain(|e| e.partition != *partition);
    }
}

/// Write `bytes` to `path` readable by the owner only, through a temporary
/// file renamed into place.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// The directory of the index and the file store: the directory of the
/// default config file, whatever `--config` names, so sessions belong to
/// the user rather than to a project file.
pub(crate) fn state_dir(spec: &CliSpec, env: &BTreeMap<String, String>) -> Option<PathBuf> {
    config::default_config_path(spec, env).and_then(|p| p.parent().map(Path::to_path_buf))
}

/// Keeps the secret of a session.
pub(crate) trait Vault: Send + Sync + fmt::Debug {
    fn read(&self, id: &str) -> Result<Option<String>, String>;
    fn write(&self, id: &str, secret: &str) -> Result<(), String>;
    fn remove(&self, id: &str) -> Result<(), String>;
}

/// The vault of `kind`; `None` for the ephemeral store.
pub(crate) fn vault(
    kind: StoreKind,
    spec: &CliSpec,
    dir: Option<&Path>,
) -> Result<Option<Arc<dyn Vault>>, String> {
    match kind {
        StoreKind::Ephemeral => Ok(None),
        StoreKind::Keychain => Ok(Some(Arc::new(Keychain::new(spec)?))),
        StoreKind::File => {
            let dir = dir.ok_or_else(|| {
                "the file session store needs a home directory (HOME, XDG_CONFIG_HOME, APPDATA or USERPROFILE)".to_string()
            })?;
            Ok(Some(Arc::new(FileVault {
                dir: dir.join("sessions"),
            })))
        }
    }
}

#[derive(Debug)]
struct FileVault {
    dir: PathBuf,
}

impl Vault for FileVault {
    fn read(&self, id: &str) -> Result<Option<String>, String> {
        let path = self.dir.join(format!("{id}.json"));
        match std::fs::read_to_string(&path) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("session file {}: {e}", path.display())),
        }
    }

    fn write(&self, id: &str, secret: &str) -> Result<(), String> {
        let path = self.dir.join(format!("{id}.json"));
        write_private(&path, secret.as_bytes())
            .map_err(|e| format!("session file {}: {e}", path.display()))
    }

    fn remove(&self, id: &str) -> Result<(), String> {
        let path = self.dir.join(format!("{id}.json"));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("session file {}: {e}", path.display())),
        }
    }
}

/// The platform keychain, driven through its command-line tool. Secrets go
/// through standard input, never the command line.
#[derive(Debug)]
struct Keychain {
    service: String,
    tool: KeychainTool,
}

#[derive(Debug, Clone, Copy)]
enum KeychainTool {
    /// macOS `security`.
    Security,
    /// libsecret `secret-tool`.
    SecretTool,
}

const UNAVAILABLE: &str = "no keychain is available";

impl Keychain {
    fn new(spec: &CliSpec) -> Result<Keychain, String> {
        let tool = if cfg!(target_os = "macos") {
            KeychainTool::Security
        } else if cfg!(any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )) {
            KeychainTool::SecretTool
        } else {
            return Err(format!(
                "{UNAVAILABLE}: this platform has no supported keychain adapter"
            ));
        };
        Ok(Keychain {
            service: format!("{}-cli-session", spec.config_dir),
            tool,
        })
    }

    fn program(&self) -> &'static str {
        match self.tool {
            KeychainTool::Security => "/usr/bin/security",
            KeychainTool::SecretTool => "secret-tool",
        }
    }

    fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<(bool, Vec<u8>), String> {
        let mut child = Command::new(self.program())
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{UNAVAILABLE}: cannot run {}: {e}", self.program()))?;
        if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(bytes)
                .map_err(|e| format!("{UNAVAILABLE}: {}: {e}", self.program()))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("{UNAVAILABLE}: {}: {e}", self.program()))?;
        Ok((out.status.success(), out.stdout))
    }
}

/// Secrets travel hex-encoded so no keychain tool has to quote them.
fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<String> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

impl Vault for Keychain {
    fn read(&self, id: &str) -> Result<Option<String>, String> {
        let (ok, out) = match self.tool {
            KeychainTool::Security => self.run(
                &["find-generic-password", "-s", &self.service, "-a", id, "-w"],
                None,
            )?,
            KeychainTool::SecretTool => {
                self.run(&["lookup", "service", &self.service, "account", id], None)?
            }
        };
        let text = String::from_utf8_lossy(&out);
        if !ok || text.trim().is_empty() {
            // Both tools fail the same way for a missing item and a locked
            // or denied keychain; a later write tells them apart.
            return Ok(None);
        }
        unhex(&text)
            .map(Some)
            .ok_or_else(|| "the keychain item of this session is not readable".to_string())
    }

    fn write(&self, id: &str, secret: &str) -> Result<(), String> {
        let encoded = hex(secret);
        let ok = match self.tool {
            KeychainTool::Security => {
                // `security -i` reads the command from standard input, which
                // keeps the secret out of the process list.
                let line = format!(
                    "add-generic-password -U -s \"{}\" -a \"{id}\" -l \"{}\" -w \"{encoded}\"\n",
                    self.service, self.service
                );
                self.run(&["-i"], Some(line.as_bytes()))?.0
            }
            KeychainTool::SecretTool => {
                let label = format!("{} session", self.service);
                self.run(
                    &[
                        "store",
                        "--label",
                        &label,
                        "service",
                        &self.service,
                        "account",
                        id,
                    ],
                    Some(encoded.as_bytes()),
                )?
                .0
            }
        };
        // Read it back: interactive `security` reports success even when an
        // item was not written, and a locked collection can refuse quietly.
        if ok && self.read(id)?.as_deref() == Some(secret) {
            Ok(())
        } else {
            Err("the keychain refused the session: it is locked, access was denied or no keychain service is running".to_string())
        }
    }

    fn remove(&self, id: &str) -> Result<(), String> {
        match self.tool {
            KeychainTool::Security => {
                self.run(
                    &["delete-generic-password", "-s", &self.service, "-a", id],
                    None,
                )?;
            }
            KeychainTool::SecretTool => {
                self.run(&["clear", "service", &self.service, "account", id], None)?;
            }
        }
        match self.read(id)? {
            None => Ok(()),
            Some(_) => Err("the keychain did not remove the session".to_string()),
        }
    }
}

/// Write, read back and remove a throwaway item, so a login fails before
/// the browser opens when the store cannot keep its result.
pub(crate) fn probe(vault: &dyn Vault) -> Result<(), String> {
    let id = format!("probe-{}", std::process::id());
    vault.write(&id, "probe")?;
    vault.remove(&id)
}

fn token_json(t: &StoredToken) -> String {
    json!({
        "access_token": t.access_token,
        "refresh_token": t.refresh_token,
        "expires_at": t.expires_at,
        "token_type": t.token_type,
        "scope": t.scope,
    })
    .to_string()
}

fn token_from_json(text: &str) -> Option<StoredToken> {
    let v: Value = serde_json::from_str(text).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(StoredToken {
        access_token: s("access_token").filter(|t| !t.is_empty())?,
        refresh_token: s("refresh_token"),
        expires_at: v.get("expires_at").and_then(Value::as_u64),
        token_type: s("token_type"),
        scope: s("scope"),
    })
}

/// One scheme's session bound into a [`SessionStore`].
#[derive(Debug)]
struct Bound {
    id: String,
    partition: Partition,
    vault: Arc<dyn Vault>,
    token: Option<StoredToken>,
}

/// The token store of a CLI run: each bound scheme's tokens come from and go
/// to its session's vault. Writes are fenced against the vault's current
/// content, so a refresh that lost a race with another process or with a
/// logout is not committed.
#[derive(Debug)]
pub(crate) struct SessionStore {
    bound: Mutex<BTreeMap<String, Bound>>,
    /// Where the index lives, to keep its expiry current after a refresh.
    index_dir: Option<PathBuf>,
}

impl SessionStore {
    pub(crate) fn new(index_dir: Option<PathBuf>) -> SessionStore {
        SessionStore {
            bound: Mutex::new(BTreeMap::new()),
            index_dir,
        }
    }

    /// Bind `scheme` to the session of `partition`, reading its token from
    /// `vault` when `load` is set.
    pub(crate) fn bind(
        &self,
        partition: Partition,
        vault: Arc<dyn Vault>,
        load: bool,
    ) -> Result<bool, String> {
        let id = partition.id();
        let token = if load {
            vault.read(&id)?.as_deref().and_then(token_from_json)
        } else {
            None
        };
        let present = token.is_some();
        self.lock().insert(
            partition.scheme.clone(),
            Bound {
                id,
                partition,
                vault,
                token,
            },
        );
        Ok(present)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Bound>> {
        self.bound.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn note_write(&self, partition: &Partition, token: Option<&StoredToken>) {
        let Some(dir) = &self.index_dir else { return };
        let Ok(mut index) = Index::load(dir) else {
            return;
        };
        let Some(entry) = index.entries.iter_mut().find(|e| e.partition == *partition) else {
            return;
        };
        match token {
            Some(t) => {
                entry.expires_at = t.expires_at;
                entry.refreshable = t.refresh_token.is_some();
                entry.granted_scope = t.scope.clone();
            }
            None => index.remove(partition),
        }
        let _ = index.save(dir);
    }

    fn current(b: &Bound) -> Result<Option<StoredToken>, String> {
        Ok(b.vault.read(&b.id)?.as_deref().and_then(token_from_json))
    }
}

impl TokenStore for SessionStore {
    fn get(&self, scheme: &str) -> Option<StoredToken> {
        self.lock().get(scheme).and_then(|b| b.token.clone())
    }

    fn set(&self, scheme: &str, token: StoredToken) {
        let mut bound = self.lock();
        let Some(b) = bound.get_mut(scheme) else {
            return;
        };
        if b.vault.write(&b.id, &token_json(&token)).is_ok() {
            let partition = b.partition.clone();
            b.token = Some(token.clone());
            drop(bound);
            self.note_write(&partition, Some(&token));
        }
    }

    fn delete(&self, scheme: &str) {
        let mut bound = self.lock();
        let Some(b) = bound.get_mut(scheme) else {
            return;
        };
        b.token = None;
        let _ = b.vault.remove(&b.id);
    }

    fn supports_fencing(&self) -> bool {
        true
    }

    fn set_if_current(
        &self,
        scheme: &str,
        token: StoredToken,
        current: &dyn Fn(Option<&StoredToken>) -> bool,
    ) -> bool {
        let mut bound = self.lock();
        let Some(b) = bound.get_mut(scheme) else {
            return false;
        };
        let Ok(now) = Self::current(b) else {
            return false;
        };
        if !current(now.as_ref()) {
            // Another process refreshed or logged out first: adopt what the
            // vault holds now.
            b.token = now;
            return false;
        }
        if b.vault.write(&b.id, &token_json(&token)).is_err() {
            return false;
        }
        let partition = b.partition.clone();
        b.token = Some(token.clone());
        drop(bound);
        self.note_write(&partition, Some(&token));
        true
    }

    fn delete_if_current(
        &self,
        scheme: &str,
        current: &dyn Fn(Option<&StoredToken>) -> bool,
    ) -> bool {
        let mut bound = self.lock();
        let Some(b) = bound.get_mut(scheme) else {
            return false;
        };
        let Ok(now) = Self::current(b) else {
            return false;
        };
        if !current(now.as_ref()) {
            b.token = now;
            return false;
        }
        if b.vault.remove(&b.id).is_err() {
            return false;
        }
        b.token = None;
        true
    }
}

/// An `authorizationCode` scheme of the API.
#[derive(Debug, Clone)]
pub(crate) struct CodeScheme {
    pub name: String,
    pub token_url: String,
    pub scopes: Vec<String>,
}

pub(crate) fn code_schemes(auth: &[AuthSchemeDescriptor]) -> Vec<CodeScheme> {
    auth.iter()
        .filter_map(|s| match s {
            AuthSchemeDescriptor::Oauth2 {
                name,
                authorization_code: Some(flow),
                ..
            } => Some(CodeScheme {
                name: name.clone(),
                token_url: flow.token_url.clone(),
                scopes: flow.scopes.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Session settings of a run: flag, then environment, then profile.
#[derive(Debug, Clone)]
pub(crate) struct SessionSettings {
    /// The account named by `--account`, `<PREFIX>_ACCOUNT` or the profile.
    pub account: Option<(String, Source)>,
    pub store: (StoreKind, Source),
}

pub(crate) fn session_settings(
    spec: &CliSpec,
    env: &BTreeMap<String, String>,
    settings: &Settings,
    account_flag: Option<&str>,
    store_flag: Option<&str>,
) -> Result<SessionSettings, String> {
    let var = |suffix: &str| format!("{}_{suffix}", spec.env_prefix);
    let nonempty = |name: &str| env.get(name).map(String::as_str).filter(|v| !v.is_empty());
    let account = if let Some(a) = account_flag {
        Some((a.to_string(), Source::Flag))
    } else if let Some(a) = nonempty(&var("ACCOUNT")) {
        Some((a.to_string(), Source::Env))
    } else {
        settings
            .profile_value("account")
            .map(|a| (a.to_string(), Source::Profile))
    };
    if let Some((a, _)) = &account {
        check_account(a)?;
    }
    let store = if let Some(s) = store_flag {
        (StoreKind::parse(s, "--store")?, Source::Flag)
    } else if let Some(s) = nonempty(&var("SESSION_STORE")) {
        (StoreKind::parse(s, &var("SESSION_STORE"))?, Source::Env)
    } else if let Some(s) = settings.profile_value("session_store") {
        (
            StoreKind::parse(s, "profile session_store")?,
            Source::Profile,
        )
    } else {
        (StoreKind::Keychain, Source::Default)
    };
    Ok(SessionSettings { account, store })
}

pub(crate) fn check_account(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.@+".contains(c))
    {
        return Err(format!(
            "account `{name}`: use 1 to 128 letters, digits and `-_.@+`"
        ));
    }
    Ok(())
}

/// The account a command uses for `scheme`: the named one, then the one the
/// profile selected with `auth switch`, then the only stored account of
/// this partition, then `default`.
pub(crate) fn effective_account(
    named: Option<&str>,
    index: &Index,
    profile: &str,
    scheme: &CodeScheme,
    client_id: &str,
    origin: &str,
) -> String {
    if let Some(a) = named {
        return a.to_string();
    }
    if let Some(a) = index.active_account(profile, &scheme.name) {
        return a.to_string();
    }
    let mut accounts: Vec<&str> = index
        .entries
        .iter()
        .filter(|e| {
            e.partition.scheme == scheme.name
                && e.partition.issuer == scheme.token_url
                && e.partition.client_id == client_id
                && e.partition.origin == origin
        })
        .map(|e| e.partition.account.as_str())
        .collect();
    accounts.sort_unstable();
    accounts.dedup();
    match accounts.as_slice() {
        [only] => (*only).to_string(),
        _ => DEFAULT_ACCOUNT.to_string(),
    }
}

/// Profile and environment keys of the session settings of `scheme`
/// (`<PREFIX>_<SCHEME>_REDIRECT_URI`, `<scheme>_issuer`).
pub(crate) fn scheme_setting(
    spec: &CliSpec,
    env: &BTreeMap<String, String>,
    settings: &Settings,
    scheme: &str,
    words: &str,
) -> Option<String> {
    let suffix = format!("{}_{words}", config::upper_words(scheme));
    env.get(&format!("{}_{suffix}", spec.env_prefix))
        .filter(|v| !v.is_empty())
        .cloned()
        .or_else(|| {
            settings
                .profile_value(&suffix.to_ascii_lowercase())
                .map(str::to_string)
        })
}

/// The profile keys this module reads, for `auth status`.
pub(crate) fn known_keys(auth: &[AuthSchemeDescriptor]) -> Vec<String> {
    let mut keys = vec!["account".to_string(), "session_store".to_string()];
    for s in code_schemes(auth) {
        let base = config::upper_words(&s.name).to_ascii_lowercase();
        keys.push(format!("{base}_redirect_uri"));
        keys.push(format!("{base}_issuer"));
    }
    keys
}
