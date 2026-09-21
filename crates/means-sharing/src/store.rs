//! Trusted checkpoints are outside immutable replica files. Files publish before pointers.
use crate::{
    crypto::{self, Identity, Public},
    snapshot::{self, Snapshot},
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use means_core::Db;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub kind: String,
    pub owner: String,
    pub recipient: String,
    pub vault: String,
    pub grant: String,
    pub grant_revision: String,
    pub revision: String,
    pub previous: String,
    pub not_before: String,
    pub expires_at: String,
    pub capability: String,
    pub payload_digest: String,
    pub ledger_head: String,
    pub ledger_count: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub manifest: Manifest,
    pub snapshot: Option<Snapshot>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub owner: String,
    pub recipient: String,
    pub vault: String,
    pub grant: String,
    pub revision: String,
    pub digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rotation {
    pub version: String,
    pub owner: String,
    pub previous: Public,
    pub next: Public,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub owner: String,
    pub recipient: Public,
    pub vault: String,
    pub not_before: String,
    pub expires_at: String,
    pub revision: String,
    pub revoked: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replica {
    pub grant: String,
    pub owner: String,
    pub vault: String,
    pub revision: String,
    pub digest: String,
    pub expires_at: String,
    pub revoked: bool,
    pub path: String,
    pub file_digest: String,
    pub manifest: Manifest,
}
impl Replica {
    /// Status labels never gate reading a previously accepted copy.
    pub fn status(&self) -> Result<&'static str> {
        if self.revoked {
            return Ok("revoked — received copy remains readable");
        }
        let expiry = DateTime::parse_from_rfc3339(&self.expires_at)?;
        if Utc::now() >= expiry {
            Ok("expired — received copy remains readable")
        } else {
            Ok("active — read-only received copy")
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Overview {
    pub identity: Option<Public>,
    pub identity_root: Option<String>,
    pub pins: Vec<(String, Public)>,
    pub grants: Vec<Grant>,
    pub replicas: Vec<Replica>,
}
#[derive(Debug, Serialize)]
pub struct Scope {
    pub vault: String,
    pub name: String,
    pub counts: std::collections::BTreeMap<String, usize>,
    pub token: String,
}
pub struct Store {
    root: PathBuf,
    conn: Connection,
}
fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
fn revision(s: &str) -> Result<i64> {
    let n: i64 = s.parse()?;
    ensure!(n > 0 && n.to_string() == s, "invalid revision");
    Ok(n)
}
fn json<T: Serialize>(v: &T) -> Result<String> {
    Ok(String::from_utf8(crypto::canonical(v)?)?)
}
fn period(start: &str, end: &str, check_now: bool) -> Result<()> {
    let a = DateTime::parse_from_rfc3339(start)?;
    let b = DateTime::parse_from_rfc3339(end)?;
    ensure!(a < b, "grant must have a positive validity period");
    if check_now {
        ensure!(Utc::now() >= a && Utc::now() < b, "grant is not currently valid");
    }
    Ok(())
}
pub fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(file.metadata()?.len() <= limit as u64, "file exceeds size limit");
    let mut data = vec![];
    file.take(limit as u64 + 1).read_to_end(&mut data)?;
    ensure!(data.len() <= limit, "file exceeds size limit");
    Ok(data)
}
pub fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    if path.exists() {
        ensure!(crypto::digest(&bounded_read(path, crypto::MAX_BUNDLE)?) == crypto::digest(data), "destination already exists with different contents");
        return Ok(());
    }
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).with_context(|| format!("create {}", path.display()))?;
    file.write_all(data)?;
    file.sync_all()?;
    sync_dir(path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")))?;
    Ok(())
}
fn canonical_destination(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Ok(std::fs::canonicalize(parent)?.join(path.file_name().context("expected a file path")?))
}
fn verify_separate_files(first: &Path, second: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let a = std::fs::metadata(first)?;
        let b = std::fs::metadata(second)?;
        ensure!((a.dev(), a.ino()) != (b.dev(), b.ino()), "identity and backup refer to the same file");
    }
    ensure!(std::fs::canonicalize(first)? != std::fs::canonicalize(second)?, "identity and backup refer to the same file");
    Ok(())
}
fn sync_dir(path: &Path) -> Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        }
        let file = root.join("trust.sqlite");
        let conn = Connection::open(&file)?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA busy_timeout=5000;
  CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
  CREATE TABLE IF NOT EXISTS pins(root TEXT PRIMARY KEY,public TEXT NOT NULL);
  CREATE TABLE IF NOT EXISTS grants(id TEXT PRIMARY KEY,data TEXT NOT NULL);
  CREATE TABLE IF NOT EXISTS outbound(grant_id TEXT PRIMARY KEY,revision INTEGER NOT NULL,digest TEXT NOT NULL,manifest TEXT NOT NULL,file TEXT NOT NULL,acknowledged INTEGER NOT NULL);
  CREATE TABLE IF NOT EXISTS replicas(grant_id TEXT PRIMARY KEY,data TEXT NOT NULL);",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { root: root.to_path_buf(), conn })
    }
    fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }
    pub fn identity_root(&self) -> Result<String> {
        self.setting("identity_root")?.context("create or restore a sharing identity first")
    }
    pub fn pin(&self, public: &Public, verified: &str) -> Result<String> {
        let fp = public.fingerprint()?;
        ensure!(fp == verified, "fingerprint does not match; verify it independently");
        self.conn.execute("INSERT INTO pins(root,public) VALUES(?1,?2) ON CONFLICT(root) DO NOTHING", params![fp, json(public)?])?;
        Ok(fp)
    }
    pub fn pinned(&self, root: &str) -> Result<Public> {
        let data: String = self.conn.query_row("SELECT public FROM pins WHERE root=?1", [root], |r| r.get(0)).optional()?.context("owner/recipient fingerprint is not pinned")?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn create_identity(&mut self, file: &Path, backup: &Path, passphrase: &str) -> Result<Public> {
        ensure!(self.setting("identity_root")?.is_none(), "identity already configured; use explicit rotation");
        ensure!(canonical_destination(file)? != canonical_destination(backup)?, "choose a separate backup path");
        ensure!(!file.exists() && !backup.exists(), "identity/backup destination already exists");
        let identity = Identity::generate();
        let encrypted = identity.protect(passphrase)?;
        write_new(file, &encrypted)?;
        write_new(backup, &encrypted)?;
        verify_separate_files(file, backup)?;
        let restored = Identity::unlock(&bounded_read(backup, crypto::MAX_IDENTITY)?, passphrase)?;
        ensure!(restored.public == identity.public, "backup verification failed");
        self.restore_identity(file, &identity.public.fingerprint()?, passphrase)?;
        Ok(identity.public)
    }
    pub fn restore_identity(&mut self, file: &Path, fingerprint: &str, passphrase: &str) -> Result<Public> {
        ensure!(self.setting("identity_root")?.is_none(), "an identity is already configured");
        let identity = Identity::unlock(&bounded_read(file, crypto::MAX_IDENTITY)?, passphrase)?;
        ensure!(identity.public.fingerprint()? == fingerprint, "restored fingerprint mismatch");
        let path = std::fs::canonicalize(file)?.to_string_lossy().to_string();
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO settings VALUES('identity_root',?1)", [fingerprint])?;
        tx.execute("INSERT INTO settings VALUES('identity_file',?1)", [path])?;
        tx.execute("INSERT INTO pins VALUES(?1,?2)", params![fingerprint, json(&identity.public)?])?;
        tx.commit()?;
        Ok(identity.public)
    }
    fn unlock(&self, passphrase: &str) -> Result<Identity> {
        let path = self.setting("identity_file")?.context("no identity configured")?;
        let identity = Identity::unlock(&bounded_read(Path::new(&path), crypto::MAX_IDENTITY)?, passphrase)?;
        ensure!(identity.public == self.pinned(&self.identity_root()?)?, "identity file does not match current pin");
        Ok(identity)
    }
    pub fn overview(&self) -> Result<Overview> {
        let root = self.setting("identity_root")?;
        let identity = root.as_ref().map(|r| self.pinned(r)).transpose()?;
        let mut q = self.conn.prepare("SELECT root,public FROM pins ORDER BY root")?;
        let pins = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .map(|r| {
                let (root, p) = r?;
                Ok((root, serde_json::from_str(&p)?))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut q = self.conn.prepare("SELECT data FROM grants ORDER BY id")?;
        let grants = q.query_map([], |r| r.get::<_, String>(0))?.map(|r| Ok(serde_json::from_str(&r?)?)).collect::<Result<_>>()?;
        let mut q = self.conn.prepare("SELECT data FROM replicas ORDER BY grant_id")?;
        let replicas = q.query_map([], |r| r.get::<_, String>(0))?.map(|r| Ok(serde_json::from_str(&r?)?)).collect::<Result<_>>()?;
        Ok(Overview { identity, identity_root: root, pins, grants, replicas })
    }
    pub fn preview(&self, db: &Db, entity: i64, recipient: &str, expires: &str) -> Result<Scope> {
        ensure!(!db.is_replica(), "cannot grant a shared replica");
        let public = self.pinned(recipient)?;
        let owner = self.identity_root()?;
        period(&now(), expires, true)?;
        let mut c = db.conn();
        let tx = c.transaction()?;
        let e = means_core::entities::get_entity(&tx, entity)?;
        let snapshot = snapshot::capture(&tx, entity)?;
        let token = crypto::digest(&crypto::canonical(&(owner, public, expires, &snapshot))?);
        Ok(Scope { vault: e.uid, name: e.name, counts: snapshot.tables.iter().map(|t| (t.kind.clone(), t.records.len())).collect(), token })
    }
    pub fn grant(&self, db: &Db, entity: i64, recipient: &str, expires: &str, confirmation: &str) -> Result<Grant> {
        let scope = self.preview(db, entity, recipient, expires)?;
        ensure!(scope.token == confirmation, "grant preview is stale; preview again");
        let grant = Grant {
            id: means_core::new_uid(),
            owner: self.identity_root()?,
            recipient: self.pinned(recipient)?,
            vault: scope.vault,
            not_before: now(),
            expires_at: expires.into(),
            revision: "1".into(),
            revoked: false,
        };
        self.conn.execute("INSERT INTO grants VALUES(?1,?2)", params![grant.id, json(&grant)?])?;
        Ok(grant)
    }
    pub fn grant_by_id(&self, id: &str) -> Result<Grant> {
        let data: String = self.conn.query_row("SELECT data FROM grants WHERE id=?1", [id], |r| r.get(0)).optional()?.context("unknown grant")?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        let mut grant = self.grant_by_id(id)?;
        if !grant.revoked {
            grant.revoked = true;
            grant.revision = (revision(&grant.revision)?.checked_add(1).context("grant revision overflow")?).to_string();
            self.conn.execute("UPDATE grants SET data=?2 WHERE id=?1", params![id, json(&grant)?])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn export(&mut self, db: &Db, id: &str, passphrase: &str, destination: &Path, revocation: bool) -> Result<Manifest> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        ensure!(!db.is_replica(), "cannot export a replica as owned books");
        let grant = self.grant_by_id(id)?;
        let identity = self.unlock(passphrase)?;
        ensure!(grant.owner == self.identity_root()?, "wrong owner identity");
        ensure!(grant.revoked == revocation, if revocation { "revoke the grant first" } else { "grant is revoked" });
        if !revocation {
            period(&grant.not_before, &grant.expires_at, true)?;
        }
        let previous: Option<(i64, String, String, String, bool)> =
            tx.query_row("SELECT revision,digest,manifest,file,acknowledged FROM outbound WHERE grant_id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).optional()?;
        if let Some((_, _, manifest, file, acknowledged)) = &previous {
            let manifest: Manifest = serde_json::from_str(manifest)?;
            if !acknowledged || (revocation && manifest.kind == "revocation") {
                ensure!((manifest.kind == "revocation") == revocation, "accept the previous receipt before sending the revocation notice");
                write_new(destination, &bounded_read(&self.root.join(file), crypto::MAX_BUNDLE)?)?;
                return Ok(manifest);
            }
        }
        let (number, prior) = previous.map(|(n, d, _, _, _)| -> Result<(i64, String)> { Ok((n.checked_add(1).context("snapshot revision overflow")?, d)) }).transpose()?.unwrap_or((1, String::new()));
        let mut c = db.conn();
        let ledger = c.transaction()?;
        let entity = means_core::entities::list_entities(&ledger, true)?.into_iter().find(|e| e.uid == grant.vault).context("grant vault is not in this ledger")?;
        let checkpoint = means_core::hashchain::verify(&ledger, entity.id)?;
        ensure!(checkpoint.first_bad_seq.is_none(), "ledger hash verification failed");
        let snapshot = if revocation { None } else { Some(snapshot::capture(&ledger, entity.id)?) };
        let manifest = Manifest {
            version: "1".into(),
            kind: if revocation { "revocation" } else { "snapshot" }.into(),
            owner: grant.owner,
            recipient: grant.recipient.fingerprint()?,
            vault: grant.vault,
            grant: grant.id,
            grant_revision: grant.revision,
            revision: number.to_string(),
            previous: prior,
            not_before: grant.not_before,
            expires_at: grant.expires_at,
            capability: "read-only".into(),
            payload_digest: snapshot::payload_digest(&snapshot)?,
            ledger_head: checkpoint.head.unwrap_or_default(),
            ledger_count: checkpoint.checked.to_string(),
        };
        let delivery = Delivery { manifest: manifest.clone(), snapshot };
        let encrypted = crypto::encrypt_to(&grant.recipient, &identity.sign(crypto::DELIVERY, &delivery)?)?;
        let digest = crypto::digest(&crypto::canonical(&manifest)?);
        let file = format!("outbound-{}.age", means_core::new_uid());
        write_new(&self.root.join(&file), &encrypted)?;
        tx.execute("INSERT INTO outbound VALUES(?1,?2,?3,?4,?5,0) ON CONFLICT(grant_id) DO UPDATE SET revision=excluded.revision,digest=excluded.digest,manifest=excluded.manifest,file=excluded.file,acknowledged=0",params![id,number,digest,json(&manifest)?,file])?;
        tx.commit()?;
        write_new(destination, &encrypted)?;
        Ok(manifest)
    }
    pub fn acknowledge(&self, id: &str, file: &Path) -> Result<Receipt> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        let grant = self.grant_by_id(id)?;
        let receipt: Receipt = crypto::verify(&grant.recipient, crypto::RECEIPT, &bounded_read(file, 64 * 1024)?)?;
        ensure!(receipt.owner == grant.owner && receipt.vault == grant.vault && receipt.grant == id && receipt.recipient == grant.recipient.fingerprint()?, "receipt binding mismatch");
        let (n, d): (i64, String) = self.conn.query_row("SELECT revision,digest FROM outbound WHERE grant_id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        ensure!(revision(&receipt.revision)? == n && receipt.digest == d, "receipt does not acknowledge the current export");
        self.conn.execute("UPDATE outbound SET acknowledged=1 WHERE grant_id=?1", [id])?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn import(&mut self, file: &Path, owner: &str, passphrase: &str, receipt_file: &Path) -> Result<Replica> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        let identity = self.unlock(passphrase)?;
        let public = self.pinned(owner)?;
        let encrypted = bounded_read(file, crypto::MAX_BUNDLE)?;
        let plain = identity.decrypt(&encrypted)?;
        let delivery: Delivery = crypto::verify(&public, crypto::DELIVERY, &plain)?;
        let m = &delivery.manifest;
        ensure!(m.version == "1" && m.capability == "read-only" && m.owner == owner && m.recipient == identity.public.fingerprint()?, "snapshot identity/capability binding mismatch");
        uuid::Uuid::parse_str(&m.grant)?;
        ensure!(!m.vault.is_empty() && m.vault.len() <= 256, "invalid vault identity");
        let n = revision(&m.revision)?;
        let gr = revision(&m.grant_revision)?;
        let revoked = m.kind == "revocation";
        ensure!(revoked || m.kind == "snapshot", "unsupported delivery kind");
        ensure!(delivery.snapshot.is_none() == revoked, "snapshot/kind mismatch");
        period(&m.not_before, &m.expires_at, false)?;
        ensure!(m.payload_digest == snapshot::payload_digest(&delivery.snapshot)?, "payload digest mismatch");
        let digest = crypto::digest(&crypto::canonical(m)?);
        let previous: Option<String> = tx.query_row("SELECT data FROM replicas WHERE grant_id=?1", [&m.grant], |r| r.get(0)).optional()?;
        let previous = previous.map(|s| serde_json::from_str::<Replica>(&s)).transpose()?;
        let receipt = Receipt { owner: owner.into(), recipient: m.recipient.clone(), vault: m.vault.clone(), grant: m.grant.clone(), revision: m.revision.clone(), digest: digest.clone() };
        let signed_receipt = identity.sign(crypto::RECEIPT, &receipt)?;
        if let Some(old) = &previous {
            ensure!(old.owner == owner && old.vault == m.vault && old.manifest.recipient == m.recipient, "grant stream binding mismatch");
            if revision(&old.revision)? == n && old.digest == digest {
                write_new(receipt_file, &signed_receipt)?;
                return Ok(old.clone());
            }
            ensure!(!old.revoked, "grant was revoked");
            ensure!(n == revision(&old.revision)?.checked_add(1).context("revision overflow")? && m.previous == old.digest, "rollback, conflict or missing predecessor");
            ensure!(m.not_before == old.manifest.not_before && m.expires_at == old.expires_at, "grant validity changed; issue a new grant");
            ensure!(gr == revision(&old.manifest.grant_revision)?.checked_add(if revoked { 1 } else { 0 }).context("grant revision overflow")?, "grant revision mismatch");
        } else {
            ensure!(n == 1 && m.previous.is_empty() && gr == 1 && !revoked, "first delivery requires an initial snapshot");
        }
        if !revoked {
            period(&m.not_before, &m.expires_at, true)?;
        }
        let (path, file_digest) = if let Some(snapshot) = &delivery.snapshot {
            let name = format!("snapshot-{}.sqlite", means_core::new_uid());
            let path = self.root.join(&name);
            write_new(&path, &[])?;
            let staging = Db::open(&path)?;
            snapshot::install(&staging, snapshot, &m.vault, &m.ledger_head, m.ledger_count.parse()?)?;
            staging.conn().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);PRAGMA journal_mode=DELETE;")?;
            drop(staging);
            std::fs::File::open(&path)?.sync_all()?;
            sync_dir(&self.root)?;
            let digest = crypto::digest(&bounded_read(&path, crypto::MAX_BUNDLE * 2)?);
            (name, digest)
        } else {
            let old = previous.as_ref().context("revocation needs an accepted snapshot")?;
            (old.path.clone(), old.file_digest.clone())
        };
        let replica = Replica {
            grant: m.grant.clone(),
            owner: owner.into(),
            vault: m.vault.clone(),
            revision: m.revision.clone(),
            digest,
            expires_at: m.expires_at.clone(),
            revoked,
            path,
            file_digest,
            manifest: m.clone(),
        };
        tx.execute("INSERT INTO replicas VALUES(?1,?2) ON CONFLICT(grant_id) DO UPDATE SET data=excluded.data", params![m.grant, json(&replica)?])?;
        tx.commit()?;
        write_new(receipt_file, &signed_receipt)?;
        Ok(replica)
    }
    pub fn open_replica(&self, id: &str) -> Result<(Db, Replica)> {
        let data: String = self.conn.query_row("SELECT data FROM replicas WHERE grant_id=?1", [id], |r| r.get(0)).optional()?.context("replica not found")?;
        let replica: Replica = serde_json::from_str(&data)?;
        ensure!(replica.path.starts_with("snapshot-") && Path::new(&replica.path).components().count() == 1, "invalid trusted snapshot path");
        ensure!(replica.digest == crypto::digest(&crypto::canonical(&replica.manifest)?), "trusted manifest mismatch");
        self.pinned(&replica.owner)?;
        let path = self.root.join(&replica.path);
        ensure!(replica.file_digest == crypto::digest(&bounded_read(&path, crypto::MAX_BUNDLE * 2)?), "replica file changed; refusing to open");
        Ok((Db::open_replica(path)?, replica))
    }
    pub fn rotate(&mut self, new_file: &Path, new_backup: &Path, old_passphrase: &str, new_passphrase: &str, proof: &Path) -> Result<Public> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        ensure!(!new_file.exists() && !new_backup.exists() && canonical_destination(new_file)? != canonical_destination(new_backup)?, "choose new separate identity and backup files");
        let pending: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM outbound WHERE acknowledged=0)", [], |r| r.get(0))?;
        ensure!(!pending, "acknowledge every pending delivery before rotating owner keys");
        let old = self.unlock(old_passphrase)?;
        let next = Identity::generate();
        let encrypted = next.protect(new_passphrase)?;
        write_new(new_file, &encrypted)?;
        write_new(new_backup, &encrypted)?;
        verify_separate_files(new_file, new_backup)?;
        ensure!(Identity::unlock(&bounded_read(new_backup, crypto::MAX_IDENTITY)?, new_passphrase)?.public == next.public, "new backup verification failed");
        let root = self.identity_root()?;
        let rotation = Rotation { version: "1".into(), owner: root.clone(), previous: old.public.clone(), next: next.public.clone() };
        write_new(proof, &old.sign(crypto::ROTATION, &rotation)?)?;
        tx.execute("UPDATE pins SET public=?2 WHERE root=?1", params![root, json(&next.public)?])?;
        tx.execute("UPDATE settings SET value=?1 WHERE key='identity_file'", [std::fs::canonicalize(new_file)?.to_string_lossy().to_string()])?;
        tx.commit()?;
        Ok(next.public)
    }
    pub fn accept_rotation(&self, owner: &str, file: &Path, confirmed_new_fingerprint: &str) -> Result<Public> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        ensure!(self.setting("identity_root")?.as_deref() != Some(owner), "rotate your own identity with the identity command");
        let old = self.pinned(owner)?;
        let rotation: Rotation = crypto::verify(&old, crypto::ROTATION, &bounded_read(file, 64 * 1024)?)?;
        ensure!(rotation.version == "1" && rotation.owner == owner && rotation.previous == old && rotation.next.fingerprint()? == confirmed_new_fingerprint, "rotation binding/fingerprint mismatch");
        self.conn.execute("UPDATE pins SET public=?2 WHERE root=?1", params![owner, json(&rotation.next)?])?;
        tx.commit()?;
        Ok(rotation.next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        owner: Store,
        recipient: Store,
        db: Db,
        entity: i64,
        owner_fp: String,
        grant: Grant,
    }
    const PASS: &str = "test passphrase only";
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("means-sharing-{}", means_core::new_uid()));
            std::fs::create_dir_all(&root).unwrap();
            let mut owner = Store::open(&root.join("owner")).unwrap();
            let mut recipient = Store::open(&root.join("recipient")).unwrap();
            let a = owner.create_identity(&root.join("a.age"), &root.join("a-backup.age"), PASS).unwrap();
            let b = recipient.create_identity(&root.join("b.age"), &root.join("b-backup.age"), PASS).unwrap();
            let owner_fp = a.fingerprint().unwrap();
            let recipient_fp = b.fingerprint().unwrap();
            owner.pin(&b, &recipient_fp).unwrap();
            recipient.pin(&a, &owner_fp).unwrap();
            let db = Db::open_memory().unwrap();
            let entity = means_core::entities::create_entity(&mut db.conn(), "Shared", "person", "PT", "EUR").unwrap().id;
            // An unrelated vault must not appear in the delivered snapshot.
            means_core::entities::create_entity(&mut db.conn(), "Private", "person", "PT", "USD").unwrap();
            let expiry = (Utc::now() + chrono::Duration::days(1)).to_rfc3339();
            let scope = owner.preview(&db, entity, &recipient_fp, &expiry).unwrap();
            let grant = owner.grant(&db, entity, &recipient_fp, &expiry, &scope.token).unwrap();
            Self { root, owner, recipient, db, entity, owner_fp, grant }
        }
        fn deliver(&mut self, name: &str) -> Replica {
            let bundle = self.root.join(format!("{name}.age"));
            let receipt = self.root.join(format!("{name}.jws"));
            self.owner.export(&self.db, &self.grant.id, PASS, &bundle, false).unwrap();
            let replica = self.recipient.import(&bundle, &self.owner_fp, PASS, &receipt).unwrap();
            self.owner.acknowledge(&self.grant.id, &receipt).unwrap();
            replica
        }
        fn forged_delivery(&self, name: &str, delivery: &Delivery) -> PathBuf {
            let identity = self.owner.unlock(PASS).unwrap();
            let encrypted = crypto::encrypt_to(&self.grant.recipient, &identity.sign(crypto::DELIVERY, delivery).unwrap()).unwrap();
            let path = self.root.join(name);
            write_new(&path, &encrypted).unwrap();
            path
        }
        fn previous_delivery(&self, name: &str) -> Delivery {
            let identity = self.recipient.unlock(PASS).unwrap();
            let data = bounded_read(&self.root.join(name), crypto::MAX_BUNDLE).unwrap();
            crypto::verify(&self.owner.pinned(&self.owner_fp).unwrap(), crypto::DELIVERY, &identity.decrypt(&data).unwrap()).unwrap()
        }
    }
    #[test]
    fn snapshot_preserves_posted_draft_reversal_tags_payees_budgets_and_reports() {
        use means_core::{accounts, budgets, entities, journal, payees, reports, AccountType, EntryInput, EntryStatus, PostingInput};
        let mut f = Fixture::new();
        {
            let mut c = f.db.conn();
            let bank = accounts::ensure_account(&c, f.entity, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap().id;
            let food = accounts::ensure_account(&c, f.entity, AccountType::Expense, &["Food"], "expense", "EUR").unwrap().id;
            let usd = accounts::ensure_account(&c, f.entity, AccountType::Asset, &["Dollar bank"], "bank", "USD").unwrap().id;
            means_core::rates::set_price(&c, "USD", "EUR", "2026-02-01".parse().unwrap(), "0.9".parse().unwrap(), "manual").unwrap();
            let change = payees::Change { id: 0, entity_id: f.entity, name: "Coffee".into(), active: true, aliases: vec!["cafe".into()] };
            let preview = payees::save(&mut c, change.clone(), None).unwrap();
            let payee = payees::save(&mut c, change, Some(&preview.token)).unwrap().payee.unwrap();
            for (n, status) in [(1, EntryStatus::Posted), (2, EntryStatus::Draft), (3, EntryStatus::Posted)] {
                let mut input = EntryInput::new(f.entity, "2026-02-02".parse().unwrap());
                input.payee = "Booked cafe text".into();
                input.status = status;
                input.postings = vec![PostingInput::new(bank, (-10).into()), PostingInput::new(food, 10.into())];
                let entry = journal::create_entry(&mut c, input).unwrap();
                means_core::tags::set_tags(&mut c, entry.id, &[("trip".into(), "Lisbon".into())]).unwrap();
                let p = payees::reassign(&mut c, 0, payee.id, Some(entry.id), None).unwrap();
                payees::reassign(&mut c, 0, payee.id, Some(entry.id), Some(&p.token)).unwrap();
                if n == 3 {
                    journal::void_entry(&mut c, entry.id, Some("2026-02-03".parse().unwrap()), "test reversal").unwrap();
                }
            }
            let mut fx = EntryInput::new(f.entity, "2026-02-02".parse().unwrap());
            fx.status = EntryStatus::Posted;
            fx.postings = vec![PostingInput::new(usd, (-10).into()), PostingInput::new(food, 9.into())];
            let purchase = journal::create_entry(&mut c, fx).unwrap();
            means_core::rates::set_price(&c, "USD", "EUR", "2026-02-03".parse().unwrap(), "0.92".parse().unwrap(), "manual").unwrap();
            c.execute("INSERT INTO imports(uid,source,account_id,checksum,created_at) VALUES(?1,'test',?2,?1,'now')", params![means_core::new_uid(), usd]).unwrap();
            let import = c.last_insert_rowid();
            c.execute(
                "INSERT INTO statement_lines(import_id,account_id,position,date,amount,currency,description,fingerprint) VALUES(?1,?2,0,'2026-02-03',1000,'USD','Refund','refund-fingerprint')",
                params![import, usd],
            )
            .unwrap();
            let line = c.last_insert_rowid();
            means_core::refunds::link(&mut c, line, purchase.id).unwrap();
            budgets::save(
                &mut c,
                None,
                budgets::BudgetInput {
                    entity_id: f.entity,
                    name: "Trip".into(),
                    scope: "tag".into(),
                    account_id: None,
                    class: None,
                    tag: Some("trip:Lisbon".into()),
                    starts_on: Some("2026-02-01".parse().unwrap()),
                    ends_on: Some("2026-02-28".parse().unwrap()),
                    amount: 100.into(),
                },
            )
            .unwrap();
        }
        f.deliver("books");
        let (db, _) = f.recipient.open_replica(&f.grant.id).unwrap();
        let entity = entities::list_entities(&db.conn(), true).unwrap()[0].id;
        fn normalized(mut v: serde_json::Value) -> serde_json::Value {
            match &mut v {
                serde_json::Value::Object(m) => {
                    m.remove("id");
                    m.remove("account_id");
                    m.remove("entity_id");
                    for v in m.values_mut() {
                        *v = normalized(v.take());
                    }
                }
                serde_json::Value::Array(a) => {
                    for v in a {
                        *v = normalized(v.take());
                    }
                }
                _ => {}
            }
            v
        }
        let owner_conn = f.db.conn();
        let copy_conn = db.conn();
        for (owner, copy) in [
            (reports::trial_balance(&owner_conn, f.entity, None).unwrap(), reports::trial_balance(&copy_conn, entity, None).unwrap()),
            (reports::income_statement(&owner_conn, f.entity, None, None).unwrap(), reports::income_statement(&copy_conn, entity, None, None).unwrap()),
            (reports::balance_sheet(&owner_conn, Some(f.entity), None, "EUR").unwrap(), reports::balance_sheet(&copy_conn, Some(entity), None, "EUR").unwrap()),
        ] {
            assert_eq!(owner.total_debit, copy.total_debit);
            assert_eq!(owner.total_credit, copy.total_credit);
            assert_eq!(owner.net, copy.net);
            assert_eq!(owner.currency, copy.currency);
            assert_eq!(normalized(serde_json::to_value(owner.summary).unwrap()), normalized(serde_json::to_value(copy.summary).unwrap()));
            assert_eq!(normalized(serde_json::to_value(owner.rows).unwrap()), normalized(serde_json::to_value(copy.rows).unwrap()));
        }
        assert_eq!(
            normalized(serde_json::to_value(budgets::list(&owner_conn, f.entity, None, None).unwrap()).unwrap()),
            normalized(serde_json::to_value(budgets::list(&copy_conn, entity, None, None).unwrap()).unwrap())
        );
        assert_eq!(copy_conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE refund_of_id IS NOT NULL", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(copy_conn.query_row("SELECT COUNT(*) FROM imports", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(copy_conn.query_row("SELECT COUNT(*) FROM statement_lines", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(journal::count_by_status(&copy_conn, "draft").unwrap(), 1);
        assert_eq!(payees::list(&copy_conn, entity).unwrap().len(), 1);
        assert_eq!(copy_conn.query_row("SELECT COUNT(*) FROM payee_aliases", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert!(means_core::hashchain::verify(&copy_conn, entity).unwrap().first_bad_seq.is_none());
    }
    #[test]
    fn relative_destinations_and_backup_aliases() {
        let relative = PathBuf::from(format!(".means-sharing-test-{}", means_core::new_uid()));
        let result = write_new(&relative, b"test data");
        let _ = std::fs::remove_file(&relative);
        result.unwrap();
        let f = Fixture::new();
        std::fs::create_dir(f.root.join("sub")).unwrap();
        let mut fresh = Store::open(&f.root.join("fresh")).unwrap();
        assert!(fresh.create_identity(&f.root.join("alias.age"), &f.root.join("sub/../alias.age"), PASS).is_err());
        assert!(!f.root.join("alias.age").exists());
        let original = f.root.join("file.age");
        let alias = f.root.join("hardlink.age");
        write_new(&original, b"not a key").unwrap();
        std::fs::hard_link(&original, &alias).unwrap();
        assert!(verify_separate_files(&original, &alias).is_err());
    }
    #[test]
    fn pending_receipt_prevents_rotation_and_preserves_retry() {
        let mut f = Fixture::new();
        let bundle = f.root.join("pending.age");
        f.owner.export(&f.db, &f.grant.id, PASS, &bundle, false).unwrap();
        assert!(f.owner.rotate(&f.root.join("next.age"), &f.root.join("next-backup.age"), PASS, PASS, &f.root.join("proof.jws")).is_err());
        assert!(!f.root.join("next.age").exists());
        let retry = f.root.join("retry.age");
        f.owner.export(&f.db, &f.grant.id, PASS, &retry, false).unwrap();
        assert_eq!(std::fs::read(bundle).unwrap(), std::fs::read(&retry).unwrap());
        let receipt = f.root.join("receipt.jws");
        f.recipient.import(&retry, &f.owner_fp, PASS, &receipt).unwrap();
        f.owner.acknowledge(&f.grant.id, &receipt).unwrap();
    }
    #[test]
    fn bridge_choice_is_stable_after_local_id_remapping() {
        use means_core::{accounts, rates, AccountType};
        let mut f = Fixture::new();
        let date = "2026-02-01".parse().unwrap();
        {
            let c = f.db.conn();
            accounts::ensure_account(&c, f.entity, AccountType::Asset, &["USD bank"], "bank", "USD").unwrap();
            for (from, to, amount) in [("USD", "ZZZ", 10), ("EUR", "ZZZ", 5), ("USD", "AAA", 20), ("EUR", "AAA", 5)] {
                rates::set_price(&c, from, to, date, amount.into(), "manual").unwrap();
            }
        }
        f.deliver("bridge");
        let (replica, _) = f.recipient.open_replica(&f.grant.id).unwrap();
        let owner = rates::rate_for(&f.db.conn(), "USD", "EUR", date).unwrap().unwrap();
        let copy = rates::rate_for(&replica.conn(), "USD", "EUR", date).unwrap().unwrap();
        assert_eq!(owner.rate, copy.rate);
        assert_eq!(copy.rate, rust_decimal::Decimal::from(4));
    }
    #[test]
    fn owner_rotation_requires_verified_proof_and_old_key_backup_recovers_identity() {
        let mut f = Fixture::new();
        f.deliver("before");
        let proof = f.root.join("rotation.jws");
        let next = f.owner.rotate(&f.root.join("next-key.age"), &f.root.join("next-backup.age"), PASS, PASS, &proof).unwrap();
        let bundle = f.root.join("rotated.age");
        f.owner.export(&f.db, &f.grant.id, PASS, &bundle, false).unwrap();
        assert!(f.recipient.import(&bundle, &f.owner_fp, PASS, &f.root.join("rotated.jws")).is_err());
        assert!(f.recipient.accept_rotation(&f.owner_fp, &proof, "wrong fingerprint").is_err());
        f.recipient.accept_rotation(&f.owner_fp, &proof, &next.fingerprint().unwrap()).unwrap();
        f.recipient.import(&bundle, &f.owner_fp, PASS, &f.root.join("rotated.jws")).unwrap();
        f.recipient.open_replica(&f.grant.id).unwrap();
        assert!(f.recipient.accept_rotation(&f.owner_fp, &proof, &next.fingerprint().unwrap()).is_err());
        let mut recovery = Store::open(&f.root.join("recovery")).unwrap();
        let p = recovery.restore_identity(&f.root.join("a-backup.age"), &f.owner_fp, PASS).unwrap();
        assert_eq!(p.fingerprint().unwrap(), f.owner_fp);
    }
    #[test]
    fn revocation_keeps_received_copy_readable_and_blocks_updates() {
        let mut f = Fixture::new();
        let initial = f.deliver("initial");
        assert_eq!(initial.status().unwrap(), "active — read-only received copy");
        f.owner.revoke(&f.grant.id).unwrap();
        assert!(f.owner.export(&f.db, &f.grant.id, PASS, &f.root.join("forbidden.age"), false).is_err());
        let notice = f.root.join("revoke.age");
        let receipt = f.root.join("revoke.jws");
        f.owner.export(&f.db, &f.grant.id, PASS, &notice, true).unwrap();
        let revoked = f.recipient.import(&notice, &f.owner_fp, PASS, &receipt).unwrap();
        f.owner.acknowledge(&f.grant.id, &receipt).unwrap();
        let retry = f.root.join("revocation-retry.age");
        let retry_manifest = f.owner.export(&f.db, &f.grant.id, PASS, &retry, true).unwrap();
        assert_eq!(retry_manifest.revision, revoked.revision);
        f.recipient.import(&retry, &f.owner_fp, PASS, &f.root.join("revocation-retry.jws")).unwrap();
        assert_eq!(revoked.path, initial.path);
        assert!(revoked.status().unwrap().starts_with("revoked"));
        let (db, again) = f.recipient.open_replica(&f.grant.id).unwrap();
        assert!(again.revoked);
        assert!(db.is_replica());
        assert_eq!(means_core::entities::list_entities(&db.conn(), true).unwrap().len(), 1);
        assert!(means_core::entities::create_entity(&mut db.conn(), "Forbidden", "person", "PT", "EUR").is_err());
        assert!(Db::open(db.path()).is_err());
        // Direct writable SQLite handles still encounter the permanent write triggers.
        let raw = Connection::open(db.path()).unwrap();
        assert!(raw.execute("DELETE FROM entities", []).is_err());
        // Replaying the revocation is harmless and can recover a lost receipt.
        f.recipient.import(&notice, &f.owner_fp, PASS, &f.root.join("receipt-retry.jws")).unwrap();
        assert!(f.recipient.import(&f.root.join("initial.age"), &f.owner_fp, PASS, &f.root.join("old.jws")).is_err());
        let mut next = f.previous_delivery("initial.age");
        next.manifest.revision = "3".into();
        next.manifest.previous = revoked.digest;
        let path = f.forged_delivery("after-revoke.age", &next);
        assert!(f.recipient.import(&path, &f.owner_fp, PASS, &f.root.join("no.jws")).is_err());
    }
    #[test]
    fn expired_received_copy_is_readable_but_new_delivery_is_rejected() {
        let mut f = Fixture::new();
        f.deliver("initial");
        // Model time passing without sleeping or altering the production clock.
        let mut old = f.recipient.overview().unwrap().replicas.remove(0);
        old.expires_at = (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
        old.manifest.expires_at = old.expires_at.clone();
        old.manifest.not_before = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        old.digest = crypto::digest(&crypto::canonical(&old.manifest).unwrap());
        f.recipient.conn.execute("UPDATE replicas SET data=?1", [json(&old).unwrap()]).unwrap();
        let (db, copy) = f.recipient.open_replica(&f.grant.id).unwrap();
        assert!(copy.status().unwrap().starts_with("expired"));
        assert_eq!(means_core::entities::list_entities(&db.conn(), true).unwrap()[0].name, "Shared");
        let mut next = f.previous_delivery("initial.age");
        next.manifest = old.manifest.clone();
        next.manifest.revision = "2".into();
        next.manifest.previous = old.digest;
        let path = f.forged_delivery("expired.age", &next);
        let error = f.recipient.import(&path, &f.owner_fp, PASS, &f.root.join("no.jws")).unwrap_err();
        assert!(error.to_string().contains("not currently valid"));
        assert_eq!(f.recipient.overview().unwrap().replicas[0].revision, "1");
    }
    #[test]
    fn failed_publication_preserves_checkpoint_and_retry_repairs_receipt() {
        let mut f = Fixture::new();
        let first = f.deliver("first");
        let bundle = f.root.join("next.age");
        let receipt = f.root.join("next.jws");
        f.owner.export(&f.db, &f.grant.id, PASS, &bundle, false).unwrap();
        f.recipient.conn.execute_batch("CREATE TRIGGER fail_publish BEFORE INSERT ON replicas BEGIN SELECT RAISE(ABORT,'simulated disk failure'); END;").unwrap();
        assert!(f.recipient.import(&bundle, &f.owner_fp, PASS, &receipt).is_err());
        assert_eq!(f.recipient.overview().unwrap().replicas[0].digest, first.digest);
        f.recipient.open_replica(&f.grant.id).unwrap();
        f.recipient.conn.execute_batch("DROP TRIGGER fail_publish").unwrap();
        let missing = f.root.join("missing/receipt.jws");
        assert!(f.recipient.import(&bundle, &f.owner_fp, PASS, &missing).is_err());
        assert_eq!(f.recipient.overview().unwrap().replicas[0].revision, "2");
        let accepted = f.recipient.import(&bundle, &f.owner_fp, PASS, &receipt).unwrap();
        assert_eq!(accepted.revision, "2");
        f.owner.acknowledge(&f.grant.id, &receipt).unwrap();
        std::fs::write(f.recipient.root.join(&accepted.path), b"corrupted").unwrap();
        assert!(f.recipient.open_replica(&f.grant.id).is_err());
    }
    #[test]
    fn malformed_snapshot_and_wrong_bindings_never_install() {
        let mut f = Fixture::new();
        f.deliver("first");
        let original = f.previous_delivery("first.age");
        for kind in ["recipient", "vault", "revision", "previous", "records", "precision"] {
            let mut bad = original.clone();
            bad.manifest.revision = "2".into();
            bad.manifest.previous = crypto::digest(&crypto::canonical(&original.manifest).unwrap());
            match kind {
                "recipient" => bad.manifest.recipient = "wrong".into(),
                "vault" => bad.manifest.vault = means_core::new_uid(),
                "revision" => bad.manifest.revision = "3".into(),
                "previous" => bad.manifest.previous = "wrong".into(),
                "records" => {
                    let t = &mut bad.snapshot.as_mut().unwrap().tables[0];
                    t.records.push(t.records[0].clone());
                }
                "precision" => {
                    bad.snapshot.as_mut().unwrap().tables[1].records[0].fields.insert("precision".into(), snapshot::Cell::Integer("99".into()));
                }
                _ => unreachable!(),
            }
            bad.manifest.payload_digest = snapshot::payload_digest(&bad.snapshot).unwrap();
            let path = f.forged_delivery(&format!("{kind}.age"), &bad);
            assert!(f.recipient.import(&path, &f.owner_fp, PASS, &f.root.join(format!("{kind}.jws"))).is_err(), "{kind}");
            assert_eq!(f.recipient.overview().unwrap().replicas[0].revision, "1");
        }
        let scope = f.owner.preview(&f.db, f.entity, &f.grant.recipient.fingerprint().unwrap(), &f.grant.expires_at).unwrap();
        assert_eq!(scope.counts["entities"], 1);
    }
}
