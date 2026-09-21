//! Read-only IMAP polling. MIME delivery receipts are the durable deduplication key.
use futures_util::TryStreamExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use means_core::imports::email::{deliver, MAX_MESSAGE_BYTES};

#[derive(Subcommand)]
pub enum Command {
    /// Poll all messages once; safe to repeat from a scheduler
    Pull {
        /// IMAP TLS server hostname (credentials: MEANS_IMAP_USERNAME and MEANS_IMAP_PASSWORD)
        #[arg(long)]
        host: String,
        #[arg(long, default_value_t = 993)]
        port: u16,
        /// Dedicated mailbox/folder to read
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        /// Default: inbox/ next to the ledger
        #[arg(long)]
        inbox: Option<PathBuf>,
        /// Fetch and validate messages without writing files
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Default, Debug, serde::Serialize)]
struct PollResult {
    messages: usize,
    received: usize,
    already_received: usize,
    attachments: usize,
    failed_uids: Vec<u32>,
    dry_run: bool,
}

pub async fn run(db_path: PathBuf, command: Command) -> Result<()> {
    let Command::Pull { host, port, mailbox, inbox, dry_run } = command;
    let username = credential("MEANS_IMAP_USERNAME")?;
    let password = credential("MEANS_IMAP_PASSWORD")?;
    let stream = timed("IMAP connection", TcpStream::connect((host.as_str(), port))).await?;
    // Platform trust and hostname verification remain mandatory.
    let tls = tokio_native_tls::TlsConnector::from(native_tls::TlsConnector::new().context("initialize TLS")?);
    let stream = timed("IMAP TLS verification or handshake", tls.connect(&host, stream)).await?;
    let mut client = async_imap::Client::new(stream);
    timed("IMAP greeting", client.read_response()).await?.context("IMAP server closed before greeting")?;
    let mut session = timed("IMAP authentication; check credentials or provider app password", client.login(&username, &password)).await?;
    let directory = inbox.unwrap_or_else(|| db_path.parent().unwrap_or_else(|| Path::new(".")).join("inbox"));
    let result = poll(&mut session, &mailbox, &directory, dry_run).await;
    // LOGOUT does not expunge; never CLOSE, STORE, MOVE or DELETE messages.
    let logout = timed("IMAP logout", session.logout()).await;
    let report = result?;
    println!("{}", serde_json::to_string(&report)?);
    if !report.failed_uids.is_empty() {
        bail!("some IMAP messages could not be delivered; inspect the failed UIDs and retry (successful receipts are retained)");
    }
    logout.map_err(|_| anyhow!("IMAP logout failed; completed deliveries are retained"))?;
    Ok(())
}

async fn timed<T, E>(operation: &str, future: impl std::future::Future<Output = std::result::Result<T, E>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(30), future).await.with_context(|| format!("{operation} timed out"))?.map_err(|_| anyhow!("{operation} failed"))
}

fn credential(name: &str) -> Result<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty()).with_context(|| format!("set {name}"))
}

async fn poll<T: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug + Send>(session: &mut async_imap::Session<T>, mailbox: &str, directory: &Path, dry_run: bool) -> Result<PollResult> {
    timed("IMAP EXAMINE", session.examine(mailbox)).await?;
    // Scan ALL, not UNSEEN: imports never depend on another mail client's read flags.
    // No UID checkpoint means UIDVALIDITY resets and moved messages cannot skip content.
    let mut uids: Vec<_> = timed("IMAP UID search", session.uid_search("ALL")).await?.into_iter().collect();
    uids.sort_unstable();
    let mut report = PollResult { messages: uids.len(), dry_run, ..Default::default() };
    for uid in uids {
        let headers = timed("IMAP size fetch", async { session.uid_fetch(uid.to_string(), "(UID RFC822.SIZE)").await?.try_collect::<Vec<_>>().await }).await?;
        let size = headers.iter().find(|f| f.uid == Some(uid)).and_then(|f| f.size);
        let Some(size) = size.filter(|s| *s as usize <= MAX_MESSAGE_BYTES) else {
            report.failed_uids.push(uid);
            continue;
        };
        // A partial PEEK caps the requested literal and leaves the Seen flag untouched.
        let query = format!("(UID BODY.PEEK[]<0.{}>)", MAX_MESSAGE_BYTES + 1);
        let fetched = timed("IMAP body fetch", async { session.uid_fetch(uid.to_string(), query).await?.try_collect::<Vec<_>>().await }).await?;
        let body = fetched.iter().find(|f| f.uid == Some(uid)).and_then(|f| f.body());
        let Some(body) = body.filter(|body| body.len() == size as usize && body.len() <= MAX_MESSAGE_BYTES) else {
            report.failed_uids.push(uid);
            continue;
        };
        match deliver(directory, body, dry_run) {
            Ok(delivery) => {
                report.attachments += delivery.filenames.len();
                if delivery.already_received {
                    report.already_received += 1;
                } else {
                    report.received += 1;
                }
            }
            Err(_) => report.failed_uids.push(uid),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    fn message() -> Vec<u8> {
        b"MIME-Version: 1.0\r\nContent-Type: text/csv\r\nContent-Disposition: attachment; filename=bank.csv\r\nContent-Transfer-Encoding: base64\r\n\r\nYSxiDQo=".to_vec()
    }

    struct Mail {
        uid: u32,
        size: usize,
        body: Vec<u8>,
    }

    // The fake server rejects every operation other than LOGIN, EXAMINE, UID
    // SEARCH ALL, UID FETCH metadata/PEEK and LOGOUT. It also changes UIDVALIDITY.
    fn receive(directory: &Path, dry_run: bool, validity: u32, mails: Vec<Mail>) -> Result<PollResult> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            stream.write_all(b"* OK test server\r\n").unwrap();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let (tag, command) = line.trim_end().split_once(' ').unwrap();
                if command == "LOGIN \"user\" \"secret\"" {
                    writeln!(stream, "{tag} OK login\r").unwrap();
                } else if command == "EXAMINE \"Receipts\"" {
                    write!(stream, "* {} EXISTS\r\n* OK [UIDVALIDITY {validity}] valid\r\n{tag} OK [READ-ONLY] examine\r\n", mails.len()).unwrap();
                } else if command == "UID SEARCH ALL" {
                    let ids = mails.iter().map(|m| m.uid.to_string()).collect::<Vec<_>>().join(" ");
                    let space = if ids.is_empty() { "" } else { " " };
                    write!(stream, "* SEARCH{space}{ids}\r\n{tag} OK search\r\n").unwrap();
                } else if command.starts_with("UID FETCH ") {
                    let uid: u32 = command.split_whitespace().nth(2).unwrap().parse().unwrap();
                    let mail = mails.iter().find(|m| m.uid == uid).unwrap();
                    if command.ends_with("(UID RFC822.SIZE)") {
                        write!(stream, "* 1 FETCH (UID {uid} RFC822.SIZE {})\r\n{tag} OK size\r\n", mail.size).unwrap();
                    } else {
                        assert_eq!(command, format!("UID FETCH {uid} (UID BODY.PEEK[]<0.{}>)", MAX_MESSAGE_BYTES + 1));
                        write!(stream, "* 1 FETCH (UID {uid} BODY[]<0> {{{}}}\r\n", mail.body.len()).unwrap();
                        stream.write_all(&mail.body).unwrap();
                        write!(stream, ")\r\n{tag} OK body\r\n").unwrap();
                    }
                } else if command == "LOGOUT" {
                    write!(stream, "* BYE logout\r\n{tag} OK logout\r\n").unwrap();
                    break;
                } else {
                    panic!("unexpected or mutating IMAP command: {command}");
                }
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = runtime.block_on(async {
            let stream = TcpStream::connect(address).await.unwrap();
            let mut client = async_imap::Client::new(stream);
            client.read_response().await.unwrap().unwrap();
            let mut session = client.login("user", "secret").await.unwrap();
            let result = poll(&mut session, "Receipts", directory, dry_run).await;
            session.logout().await.unwrap();
            result
        });
        server.join().unwrap();
        result
    }

    fn mail(uid: u32) -> Mail {
        let body = message();
        Mail { uid, size: body.len(), body }
    }

    #[test]
    fn read_only_poll_dry_run_retries_and_uidvalidity_changes() {
        let root = std::env::temp_dir().join(format!("means-imap-{}", uuid::Uuid::new_v4()));
        let first = receive(&root, true, 1, vec![mail(7)]).unwrap();
        assert_eq!(first.received, 1);
        assert_eq!(first.attachments, 1);
        assert!(!root.exists());
        let first = receive(&root, false, 1, vec![mail(7)]).unwrap();
        assert_eq!(first.received, 1);
        let attachment = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|x| x == "csv")).unwrap();
        assert_eq!(std::fs::read(&attachment).unwrap(), b"a,b\r\n");
        std::fs::remove_file(&attachment).unwrap();
        // Simulate a mailbox rebuild and a changed UID for the same message.
        let repeat = receive(&root, false, 2, vec![mail(20)]).unwrap();
        assert_eq!(repeat.already_received, 1);
        assert_eq!(repeat.received, 0);
        assert!(!attachment.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_and_oversized_messages_do_not_block_other_messages_or_retries() {
        let root = std::env::temp_dir().join(format!("means-imap-bad-{}", uuid::Uuid::new_v4()));
        let bad = b"Content-Type: text/csv\r\nContent-Disposition: attachment; filename=bad.csv\r\nContent-Transfer-Encoding: unknown\r\n\r\nsecret".to_vec();
        let result = receive(
            &root,
            false,
            1,
            vec![Mail { uid: 1, size: MAX_MESSAGE_BYTES + 1, body: vec![] }, Mail { uid: 2, size: bad.len(), body: bad }, Mail { uid: 3, size: 999, body: message() }, mail(4)],
        )
        .unwrap();
        assert_eq!(result.failed_uids, [1, 2, 3]);
        assert_eq!(result.received, 1);
        let result = receive(&root, false, 1, vec![mail(1), mail(4)]).unwrap();
        assert!(result.failed_uids.is_empty());
        assert_eq!(result.already_received, 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_mailbox_writes_nothing() {
        let root = std::env::temp_dir().join(format!("means-imap-empty-{}", uuid::Uuid::new_v4()));
        let result = receive(&root, false, 1, vec![]).unwrap();
        assert_eq!(result.messages, 0);
        assert!(!root.exists());
    }
}
