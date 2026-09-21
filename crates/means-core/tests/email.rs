use means_core::imports::{checksum, email};
fn multipart(parts: &[&str]) -> Vec<u8> {
    let mut message = String::from("From: bank@example.test\r\nMessage-ID: <same-id@example.test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n");
    for part in parts {
        message.push_str("--outer\r\n");
        message.push_str(part);
        message.push_str("\r\n");
    }
    message.push_str("--outer--\r\n");
    message.into_bytes()
}
#[test]
fn decodes_base64_and_quoted_printable_without_charset_or_newline_changes() {
    let raw = multipart(&[
        "Content-Type: text/plain\r\n\r\nA message body is not a statement.",
        "Content-Type: text/csv; charset=iso-8859-1\r\nContent-Disposition: attachment; filename=statement.csv\r\nContent-Transfer-Encoding: base64\r\n\r\nQ2Fm6SwxMC4wMA0K",
        "Content-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=binary.dat\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n=00=FF=0D=0A=3D",
    ]);
    let mail = email::extract(&raw).unwrap();
    assert_eq!(mail.checksum, checksum(&raw));
    assert_eq!(mail.attachments.len(), 2);
    assert_eq!(mail.attachments[0].content, b"Caf\xe9,10.00\r\n");
    assert_eq!(mail.attachments[1].content, b"\0\xff\r\n=");
    assert_eq!(mail.attachments[0].checksum, checksum(&mail.attachments[0].content));
}
#[test]
fn finds_nested_named_inline_parts_and_keeps_unknown_formats() {
    let raw = multipart(&[
        "Content-Type: multipart/alternative; boundary=inner\r\n\r\n--inner\r\nContent-Type: text/html\r\n\r\n<p>Body</p>\r\n--inner\r\nContent-Type: application/pdf; name=statement.pdf\r\nContent-Disposition: inline\r\n\r\n%PDF-test\r\n--inner--",
        "Content-Type: application/zip\r\nContent-Disposition: attachment; filename=archive.zip\r\n\r\nPK-test",
    ]);
    let mail = email::extract(&raw).unwrap();
    assert_eq!(mail.attachments.len(), 2);
    assert_eq!(mail.attachments[0].content, b"%PDF-test");
    assert_eq!(mail.attachments[1].content, b"PK-test");
    assert!(mail.attachments[1].inbox_filename.ends_with("archive.zip"));
}
#[test]
fn filenames_are_local_unique_and_stable_without_trusting_message_id() {
    let part = "Content-Type: text/csv\r\nContent-Disposition: attachment; filename=\"../../C:\\secret\\statement.csv\"\r\n\r\na,b";
    let raw = multipart(&[part, part]);
    let mail = email::extract(&raw).unwrap();
    assert_ne!(mail.attachments[0].inbox_filename, mail.attachments[1].inbox_filename);
    for attachment in &mail.attachments {
        let path = std::path::Path::new(&attachment.inbox_filename);
        assert_eq!(path.components().count(), 1);
        assert!(!attachment.inbox_filename.contains('\\'));
        assert!(attachment.inbox_filename.ends_with(".csv"));
        assert_eq!(attachment.content, b"a,b");
    }
    assert_eq!(email::extract(&raw).unwrap().attachments[0].inbox_filename, mail.attachments[0].inbox_filename);
    let changed = multipart(&["Content-Type: text/csv\r\nContent-Disposition: attachment; filename=statement.csv\r\n\r\nc,d"]);
    assert_ne!(email::extract(&changed).unwrap().checksum, mail.checksum);
}
#[test]
fn decodes_extended_filenames_and_assigns_names_to_unnamed_attachments() {
    let raw = multipart(&[
        "Content-Type: text/csv\r\nContent-Disposition: attachment; filename*=utf-8''caf%C3%A9.csv\r\n\r\na,b",
        "Content-Type: application/x-ofx\r\nContent-Disposition: attachment\r\n\r\n<OFX>data</OFX>",
    ]);
    let mail = email::extract(&raw).unwrap();
    assert_eq!(mail.attachments[0].original_filename, "café.csv");
    assert!(mail.attachments[0].inbox_filename.ends_with("café.csv"));
    assert!(mail.attachments[1].original_filename.is_empty());
    assert!(mail.attachments[1].inbox_filename.ends_with("attachment.ofx"));
}
#[test]
fn rejects_incomplete_or_unsupported_messages_as_a_whole() {
    let good = "Content-Disposition: attachment; filename=a.csv\r\n\r\na,b";
    for bad in [
        "Content-Disposition: attachment; filename=b.csv\r\nContent-Transfer-Encoding: unknown\r\n\r\nsecret",
        "Content-Disposition: attachment; filename=b.csv\r\nContent-Transfer-Encoding: base64\r\nContent-Transfer-Encoding: binary\r\n\r\nYQ==",
        "Content-Disposition: attachment; filename=b.csv\r\nContent-Transfer-Encoding: base64\r\n\r\nA",
    ] {
        assert!(email::extract(&multipart(&[good, bad])).is_err());
    }
    let raw = multipart(&[good]);
    let truncated = &raw[..raw.len() - "--outer--\r\n".len()];
    assert!(email::extract(truncated).is_err());
    assert!(email::extract(b"").is_err());
    assert!(email::extract(&multipart(&vec![good; 101])).is_err());
}
#[test]
fn keeps_attached_messages_as_files_and_does_not_execute_or_expand_them() {
    let raw = multipart(&["Content-Type: message/rfc822\r\nContent-Disposition: attachment; filename=forward.eml\r\n\r\nSubject: forwarded\r\n\r\nbody"]);
    let mail = email::extract(&raw).unwrap();
    assert_eq!(mail.attachments.len(), 1);
    assert_eq!(mail.attachments[0].content, b"Subject: forwarded\r\n\r\nbody");
    assert!(email::extract(b"Subject: plain message\r\n\r\nhello").unwrap().attachments.is_empty());
}

#[test]
fn accepts_case_and_header_whitespace_without_returning_encoded_bytes() {
    let raw = multipart(&["Content-Type: text/csv\r\nContent-Disposition: attachment; filename=a.csv\r\nContent-Transfer-Encoding: BASE64 \t\r\n\r\nYSxi"]);
    assert_eq!(email::extract(&raw).unwrap().attachments[0].content, b"a,b");
}

fn temp() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("means-email-{}", means_core::new_uid()))
}
#[test]
fn delivery_keeps_raw_evidence_and_does_not_replay_consumed_attachments() {
    let raw = multipart(&["Content-Type: text/csv\r\nContent-Disposition: attachment; filename=a.csv\r\n\r\na,b"]);
    let dir = temp();
    let dry = email::deliver(&dir, &raw, true).unwrap();
    assert!(!dry.already_received && !dir.exists());
    let delivered = email::deliver(&dir, &raw, false).unwrap();
    let path = dir.join(&delivered.filenames[0]);
    assert_eq!(std::fs::read(&path).unwrap(), b"a,b");
    assert_eq!(std::fs::read(dir.join(".email").join(format!("{}.eml", delivered.message_checksum))).unwrap(), raw);
    let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(".email").join(format!("{}.json", delivered.message_checksum))).unwrap()).unwrap();
    assert_eq!(receipt["attachments"][0]["original_filename"], "a.csv");
    std::fs::remove_file(path).unwrap(); // The inbox has consumed this file.
    assert!(email::deliver(&dir, &raw, false).unwrap().already_received);
    assert!(!dir.join(&delivered.filenames[0]).exists());
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn delivery_does_not_overwrite_files_and_retry_completes_after_failure() {
    let raw = multipart(&["Content-Disposition: attachment; filename=a.csv\r\n\r\nfirst", "Content-Disposition: attachment; filename=b.csv\r\n\r\nsecond"]);
    let dir = temp();
    std::fs::create_dir_all(&dir).unwrap();
    let plan = email::deliver(&dir, &raw, true).unwrap();
    let conflict = dir.join(&plan.filenames[1]);
    std::fs::write(&conflict, b"keep this file").unwrap();
    assert!(email::deliver(&dir, &raw, false).is_err());
    assert_eq!(std::fs::read(&conflict).unwrap(), b"keep this file");
    assert!(!dir.join(".email").join(format!("{}.json", plan.message_checksum)).exists());
    std::fs::remove_file(conflict).unwrap();
    assert!(!email::deliver(&dir, &raw, false).unwrap().already_received);
    assert_eq!(std::fs::read(dir.join(&plan.filenames[1])).unwrap(), b"second");
    assert!(email::deliver(&dir, &raw, false).unwrap().already_received);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn invalid_message_does_not_publish_any_good_earlier_attachment() {
    let raw = multipart(&["Content-Disposition: attachment; filename=a.csv\r\n\r\ngood", "Content-Disposition: attachment; filename=b.csv\r\nContent-Transfer-Encoding: unknown\r\n\r\nbad"]);
    let dir = temp();
    assert!(email::deliver(&dir, &raw, false).is_err());
    assert!(!dir.exists());
}

#[test]
fn routing_requires_matching_message_and_attachment_evidence() {
    let dir = temp();
    let raw = multipart(&["Content-Type: text/csv\r\nContent-Disposition: attachment; filename=\"../Bank Statement.csv\"\r\n\r\na,b"]);
    let delivery = email::deliver(&dir, &raw, false).unwrap();
    let stored = &delivery.filenames[0];
    assert_eq!(email::routing_filename(&dir, stored, b"a,b").unwrap().as_deref(), Some("Bank Statement.csv"));
    assert!(email::routing_filename(&dir, stored, b"changed").is_err());
    let evidence = dir.join(".email").join(format!("{}.eml", delivery.message_checksum));
    std::fs::write(&evidence, b"Subject: changed\r\n\r\nbody").unwrap();
    assert!(email::routing_filename(&dir, stored, b"a,b").is_err());
    std::fs::remove_file(evidence).unwrap();
    assert!(email::routing_filename(&dir, stored, b"a,b").is_err());
    assert_eq!(email::routing_filename(&dir, "statement.csv", b"a,b").unwrap(), None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn mime_part_budget_stops_before_parsing_the_next_header() {
    let body = "Content-Type: text/plain\r\n\r\nbody";
    let valid = multipart(&vec![body; 255]);
    assert_eq!(mailparse::parse_mail(&valid).unwrap().parts().count(), 256);
    assert!(email::extract(&valid).unwrap().attachments.is_empty());
    let mut parts = vec![body; 255];
    parts.push(" invalid header\r\n\r\nbody");
    let oversized = multipart(&parts);
    assert!(mailparse::parse_mail(&oversized).is_err());
    let error = email::extract(&oversized).unwrap_err();
    assert!(error.to_string().contains("256 MIME parts"), "{error}");
}

fn nested_message(levels: usize, leaf: &str) -> String {
    let mut result = leaf.to_owned();
    for level in 0..levels {
        result = format!("Content-Type: multipart/mixed; boundary=level{level}\r\n\r\n--level{level}\r\n{result}\r\n--level{level}--\r\n");
    }
    result
}

#[test]
fn mime_depth_budget_matches_the_parser_limit() {
    let valid = nested_message(100, "Content-Type: text/plain\r\n\r\nbody");
    assert_eq!(mailparse::parse_mail(valid.as_bytes()).unwrap().parts().count(), 101);
    assert!(email::extract(valid.as_bytes()).is_ok());
    let too_deep = nested_message(101, " invalid header\r\n\r\nbody");
    let error = email::extract(too_deep.as_bytes()).unwrap_err();
    assert!(error.to_string().contains("100 MIME nesting levels"), "{error}");
}

#[test]
fn mime_part_budget_counts_nested_parts_and_boundary_prefixes() {
    let leaf = "Content-Type: text/plain\r\n\r\nbody";
    let nested = nested_message(1, leaf);
    let valid = multipart(&vec![nested.as_str(); 127]);
    assert_eq!(mailparse::parse_mail(&valid).unwrap().parts().count(), 255);
    assert!(email::extract(&valid).is_ok());
    let excessive = multipart(&vec![nested.as_str(); 128]);
    assert!(email::extract(&excessive).unwrap_err().to_string().contains("256 MIME parts"));
    // mailparse treats these delimiter prefixes as boundaries too.
    let prefixes = String::from_utf8(multipart(&vec![leaf; 256])).unwrap().replace("--outer\r\n", "--outer-extra\r\n");
    assert_eq!(mailparse::parse_mail(prefixes.as_bytes()).unwrap().parts().count(), 257);
    assert!(email::extract(prefixes.as_bytes()).unwrap_err().to_string().contains("256 MIME parts"));
}

#[test]
fn opaque_bodies_do_not_consume_mime_part_budget() {
    let opaque = nested_message(110, "Content-Type: text/plain\r\n\r\nbody");
    let attached = format!("Content-Type: message/rfc822\r\nContent-Disposition: attachment; filename=forward.eml\r\n\r\n{opaque}");
    let body = format!("Content-Type: text/plain\r\n\r\n{}", "--not-a-boundary\r\n".repeat(300));
    let raw = multipart(&[&attached, &body]);
    let mail = email::extract(&raw).unwrap();
    assert_eq!(mail.attachments.len(), 1);
    assert_eq!(mail.attachments[0].content, opaque.as_bytes());
}

#[test]
fn mime_headers_have_limits_before_tree_allocation() {
    let mut headers = "X-Test: value\r\n".repeat(4096);
    headers.push_str("\r\nbody");
    assert!(email::extract(headers.as_bytes()).is_ok());
    let too_many = format!("X-Test: one more\r\n{headers}");
    assert!(email::extract(too_many.as_bytes()).unwrap_err().to_string().contains("4096 MIME headers"));
    let too_large = format!("Content-Type: text/plain; name=\"{}\"\r\n\r\nbody", "x".repeat(64 * 1024));
    assert!(email::extract(too_large.as_bytes()).unwrap_err().to_string().contains("64 KiB"));
    let repeated = format!("{}\r\nbody", "X-Test: x\r\n".repeat(2100));
    assert!(email::extract(&multipart(&[&repeated, &repeated])).unwrap_err().to_string().contains("4096 MIME headers"));
}
