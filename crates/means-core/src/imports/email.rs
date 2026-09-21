//! Read MIME attachments without changing their decoded bytes.
//! Store message evidence before publishing attachments to the inbox.
use crate::{Error, Result};
use mailparse::{DispositionType, MailHeaderMap, ParsedMail};

pub const MAX_MESSAGE_BYTES: usize = 50 * 1024 * 1024;
const MAX_PARTS: usize = 256;
const MAX_ATTACHMENTS: usize = 100;
const MAX_DEPTH: usize = 100;
const MAX_HEADERS: usize = 4096;
const MAX_PART_HEADER_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct Attachment {
    pub part: usize,
    pub original_filename: String,
    pub content_type: String,
    pub checksum: String,
    pub content: Vec<u8>,
    /// A local basename. The MIME filename must never be used as a path.
    pub inbox_filename: String,
}

#[derive(Debug)]
pub struct Message {
    /// The hash covers the whole received message, not its untrusted Message-ID.
    pub checksum: String,
    pub attachments: Vec<Attachment>,
}

fn unnamed_extension(mime: &str) -> &'static str {
    match mime {
        "text/csv" => "csv",
        "application/x-ofx" | "application/ofx" => "ofx",
        "application/x-qfx" => "qfx",
        "application/json" => "json",
        "application/xml" | "text/xml" => "xml",
        "application/pdf" => "pdf",
        "message/rfc822" => "eml",
        _ => "bin",
    }
}

fn safe_name(original: &str, mime: &str) -> String {
    let basename = original.rsplit(['/', '\\']).next().unwrap_or("");
    let (stem, extension) = basename.rsplit_once('.').unwrap_or((basename, ""));
    let mut safe = String::new();
    for ch in stem.chars() {
        let ch = if ch.is_alphanumeric() || matches!(ch, '-' | '_') { ch } else { '_' };
        if safe.len() + ch.len_utf8() > 100 {
            break;
        }
        safe.push(ch);
    }
    if safe.trim_matches('_').is_empty() {
        safe = "attachment".into();
    }
    let ext = if !extension.is_empty() && extension.len() <= 16 && extension.bytes().all(|b| b.is_ascii_alphanumeric()) { extension } else { unnamed_extension(mime) };
    format!("{safe}.{ext}")
}

/// Recover a routing name only when the stored message proves its content.
pub fn routing_filename(directory: &std::path::Path, stored: &str, content: &[u8]) -> Result<Option<String>> {
    let Some((hash, _)) = stored.strip_prefix("email-").and_then(|s| s.split_once('-')) else {
        return Ok(None);
    };
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let path = directory.join(".email").join(format!("{hash}.eml"));
    let metadata = std::fs::symlink_metadata(&path).map_err(anyhow::Error::from)?;
    if !metadata.is_file() || metadata.len() > MAX_MESSAGE_BYTES as u64 {
        return Err(Error::Invalid("email evidence is not a valid message file".into()));
    }
    let raw = std::fs::read(path).map_err(anyhow::Error::from)?;
    let message = extract(&raw)?;
    if message.checksum != hash {
        return Err(Error::Conflict("email evidence checksum differs".into()));
    }
    let attachment =
        message.attachments.into_iter().find(|a| a.inbox_filename == stored && a.content == content).ok_or_else(|| Error::Conflict("email evidence does not contain this attachment".into()))?;
    let name: String = attachment.original_filename.rsplit(['/', '\\']).next().unwrap_or("").chars().filter(|c| !c.is_control()).collect();
    Ok(Some(if name.is_empty() { safe_name("", &attachment.content_type) } else { name }))
}

fn complete_multipart(part: &ParsedMail<'_>) -> Result<()> {
    let boundary = part.ctype.params.get("boundary").filter(|b| !b.is_empty()).ok_or_else(|| Error::Parse("multipart email has no boundary".into()))?;
    let closing = format!("--{boundary}--");
    let closed = part.raw_bytes.split(|b| *b == b'\n').any(|line| {
        let end = line.iter().rposition(|b| !matches!(b, b'\r' | b' ' | b'\t')).map(|i| i + 1).unwrap_or(0);
        &line[..end] == closing.as_bytes()
    });
    if !closed {
        return Err(Error::Parse("multipart email is incomplete: closing boundary is missing".into()));
    }
    Ok(())
}

/// Check the tree budget before mailparse allocates its parts and header vectors.
/// Use the same header parser and boundary rules as mailparse 0.17.
fn check_mime_budget(content: &[u8], depth: usize, parts: &mut usize, headers: &mut usize) -> Result<()> {
    if *parts >= MAX_PARTS {
        return Err(Error::Invalid("email exceeds 256 MIME parts".into()));
    }
    if depth > MAX_DEPTH {
        return Err(Error::Invalid("email exceeds 100 MIME nesting levels".into()));
    }
    *parts += 1;
    let mut body = 0;
    let mut content_type = None;
    while body < content.len() {
        if content[body] == b'\n' {
            body += 1;
            break;
        }
        if content[body] == b'\r' {
            if content.get(body + 1) != Some(&b'\n') {
                return Err(Error::Parse("invalid MIME email".into()));
            }
            body += 2;
            break;
        }
        if *headers >= MAX_HEADERS {
            return Err(Error::Invalid("email exceeds 4096 MIME headers".into()));
        }
        let (header, used) = mailparse::parse_header(&content[body..]).map_err(|_| Error::Parse("invalid MIME email".into()))?;
        body += used;
        if body > MAX_PART_HEADER_BYTES {
            return Err(Error::Invalid("email MIME part headers exceed 64 KiB".into()));
        }
        *headers += 1;
        if content_type.is_none() && header.get_key_raw().eq_ignore_ascii_case(b"Content-Type") {
            content_type = Some(mailparse::parse_content_type(&header.get_value()));
        }
    }
    if body > MAX_PART_HEADER_BYTES {
        return Err(Error::Invalid("email MIME part headers exceed 64 KiB".into()));
    }
    // Without Content-Type, mailparse uses text/plain or message/rfc822.
    // Neither type has subparts. Attached messages remain opaque files.
    let Some(content_type) = content_type else { return Ok(()) };
    if body == content.len() || !content_type.mimetype.starts_with("multipart") {
        return Ok(());
    }
    let Some(boundary) = content_type.params.get("boundary") else { return Ok(()) };
    let boundary = format!("--{boundary}");
    let Some(start) = boundary_start(content, body, boundary.as_bytes()) else { return Ok(()) };
    let mut boundary_end = start + boundary.len();
    while let Some(newline) = content[boundary_end..].iter().position(|b| *b == b'\n') {
        let start = boundary_end + newline + 1;
        let next = boundary_start(content, start, boundary.as_bytes());
        let mut end = next.unwrap_or(content.len());
        // mailparse removes the CRLF that belongs to the next boundary.
        if next.is_some() && end > start && content[end - 1] == b'\n' {
            end -= 1;
            if end > start && content[end - 1] == b'\r' {
                end -= 1;
            }
        }
        check_mime_budget(&content[start..end], depth + 1, parts, headers)?;
        boundary_end = next.map(|n| n + boundary.len()).unwrap_or(content.len());
        if boundary_end + 2 > content.len() || &content[boundary_end..boundary_end + 2] == b"--" {
            break;
        }
    }
    Ok(())
}

/// mailparse accepts a boundary prefix at the search start or after a newline.
/// Match that rule so malformed delimiter suffixes cannot bypass the budget.
fn boundary_start(content: &[u8], mut start: usize, boundary: &[u8]) -> Option<usize> {
    loop {
        if content[start..].starts_with(boundary) {
            return Some(start);
        }
        start += content[start..].iter().position(|b| *b == b'\n')? + 1;
    }
}

/// Decode transfer encodings only. Do not decode charsets, normalize newlines,
/// open archives, or treat the message body as a bank statement.
pub fn extract(content: &[u8]) -> Result<Message> {
    if content.is_empty() || content.len() > MAX_MESSAGE_BYTES {
        return Err(Error::Invalid("email must contain between 1 byte and 50 MiB".into()));
    }
    check_mime_budget(content, 0, &mut 0, &mut 0)?;
    let root = mailparse::parse_mail(content).map_err(|_| Error::Parse("invalid MIME email".into()))?;
    let checksum = super::checksum(content);
    let mut result = Message { checksum, attachments: Vec::new() };
    let mut decoded_bytes = 0usize;
    for (part_number, part) in root.parts().enumerate() {
        if part_number >= MAX_PARTS {
            return Err(Error::Invalid("email exceeds 256 MIME parts".into()));
        }
        if part.ctype.mimetype.starts_with("multipart/") {
            complete_multipart(part)?;
            continue;
        }
        let disposition = part.get_content_disposition();
        let filename = disposition.params.get("filename").or_else(|| part.ctype.params.get("name"));
        if disposition.disposition != DispositionType::Attachment && filename.is_none() {
            continue;
        }
        if result.attachments.len() >= MAX_ATTACHMENTS {
            return Err(Error::Invalid("email exceeds 100 attachments".into()));
        }
        let encodings = part.headers.get_all_values("Content-Transfer-Encoding");
        if encodings.len() > 1 || encodings.first().is_some_and(|s| !matches!(s.trim().to_ascii_lowercase().as_str(), "base64" | "quoted-printable" | "7bit" | "8bit" | "binary")) {
            return Err(Error::Parse("email attachment has an unsupported or ambiguous transfer encoding".into()));
        }
        // mailparse does not trim this header before it chooses a decoder.
        // Normalize the token, then decode the original body bytes once.
        use mailparse::body::Body;
        let body = part.get_body_encoded();
        let raw = match &body {
            Body::Base64(b) | Body::QuotedPrintable(b) => b.get_raw(),
            Body::SevenBit(b) | Body::EightBit(b) => b.get_raw(),
            Body::Binary(b) => b.get_raw(),
        };
        let encoding = encodings.first().map(|s| s.trim().to_ascii_lowercase());
        let bytes = match Body::new(raw, &part.ctype, &encoding) {
            Body::Base64(b) | Body::QuotedPrintable(b) => b.get_decoded().map_err(|_| Error::Parse("cannot decode email attachment transfer encoding".into()))?,
            Body::SevenBit(b) | Body::EightBit(b) => b.get_raw().to_vec(),
            Body::Binary(b) => b.get_raw().to_vec(),
        };
        decoded_bytes = decoded_bytes.checked_add(bytes.len()).ok_or_else(|| Error::Invalid("email attachment size overflow".into()))?;
        if decoded_bytes > MAX_MESSAGE_BYTES {
            return Err(Error::Invalid("decoded email attachments exceed 50 MiB".into()));
        }
        let original_filename = filename.cloned().unwrap_or_default();
        let content_type = part.ctype.mimetype.clone();
        let inbox_filename = format!("email-{}-{}-{}", result.checksum, part_number, safe_name(&original_filename, &content_type));
        result.attachments.push(Attachment { part: part_number, original_filename, content_type, checksum: super::checksum(&bytes), content: bytes, inbox_filename });
    }
    Ok(result)
}

#[derive(Debug)]
pub struct Delivery {
    pub message_checksum: String,
    pub filenames: Vec<String>,
    pub already_received: bool,
}

fn matches_file(path: &std::path::Path, content: &[u8]) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(Error::Other(e.into())),
    };
    if !metadata.is_file() || metadata.len() != content.len() as u64 || std::fs::read(path).map_err(anyhow::Error::from)? != content {
        return Err(Error::Conflict(format!("email output already exists with different content: {}", path.display())));
    }
    Ok(true)
}

/// Publish a complete file without replacing an existing name.
fn publish_file(path: &std::path::Path, content: &[u8]) -> Result<()> {
    use std::io::Write;
    if matches_file(path, content)? {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| Error::Invalid("email output has no parent directory".into()))?;
    std::fs::create_dir_all(parent).map_err(anyhow::Error::from)?;
    let temporary = parent.join(format!(".email-{}.part", crate::new_uid()));
    let write = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(content)?;
        file.sync_all()?;
        // A hard link makes publication atomic and refuses to overwrite a name.
        std::fs::hard_link(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    match write {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if matches_file(path, content)? {
                Ok(())
            } else {
                Err(Error::Conflict("email output changed during publication; retry delivery".into()))
            }
        }
        Err(e) => Err(Error::Other(e.into())),
    }
}

/// Store the message and publish its attachments. The receipt follows the files.
/// A retry can replay files after a crash; normal inbox checks prevent duplicate postings.
pub fn deliver(directory: &std::path::Path, content: &[u8], dry_run: bool) -> Result<Delivery> {
    let message = extract(content)?;
    let manifest = serde_json::json!({
        "version":1,"message_checksum":message.checksum,
        "attachments":message.attachments.iter().map(|a| serde_json::json!({
            "part":a.part,"original_filename":a.original_filename,"content_type":a.content_type,
            "checksum":a.checksum,"filename":a.inbox_filename,"bytes":a.content.len()
        })).collect::<Vec<_>>()
    });
    let receipt = serde_json::to_vec(&manifest)?;
    let storage = directory.join(".email");
    let receipt_path = storage.join(format!("{}.json", message.checksum));
    let original_path = storage.join(format!("{}.eml", message.checksum));
    let already_received = matches_file(&receipt_path, &receipt)?;
    if already_received && !matches_file(&original_path, content)? {
        return Err(Error::Conflict("email receipt exists but the original message is missing".into()));
    }
    let report = Delivery { message_checksum: message.checksum, filenames: message.attachments.iter().map(|a| a.inbox_filename.clone()).collect(), already_received };
    if dry_run || already_received {
        return Ok(report);
    }
    // Decode and validate every part before any filesystem writes above this point.
    publish_file(&original_path, content)?;
    for attachment in message.attachments {
        publish_file(&directory.join(attachment.inbox_filename), &attachment.content)?;
    }
    // Also flush names reused after an interrupted delivery.
    #[cfg(unix)]
    std::fs::File::open(directory).and_then(|d| d.sync_all()).map_err(anyhow::Error::from)?;
    publish_file(&receipt_path, &receipt)?;
    Ok(report)
}
