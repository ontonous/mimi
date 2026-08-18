//! `mimi wire` — Component Wire envelope encode/decode CLI.
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use mimi::component::{WireEnvelope, WireSchema};

use super::WireAction;

pub(crate) fn run(action: WireAction) -> Result<(), String> {
    match action {
        WireAction::Encode { input, output } => {
            let payload = read_input(&input)?;
            let envelope = WireEnvelope::new(payload);
            write_bytes(output.as_deref(), &envelope.to_bytes())
        }
        WireAction::Decode { input, output } => {
            let data = read_input(&input)?;
            let envelope = WireEnvelope::from_bytes(&data)
                .map_err(|e| format!("invalid wire envelope {}: {}", input.display(), e))?;
            write_bytes(output.as_deref(), &envelope.payload)
        }
        WireAction::ValidateSchema { input } => {
            let text = read_text(&input)?;
            let schema: WireSchema = serde_json::from_str(&text)
                .map_err(|e| format!("invalid wire schema {}: {}", input.display(), e))?;
            let errors = schema.validate();
            if errors.is_empty() {
                println!(
                    "valid wire schema: name={}, version={}, fields={}",
                    schema.name,
                    schema.version,
                    schema.fields.len()
                );
                Ok(())
            } else {
                for error in errors {
                    eprintln!("schema error: {error:?}");
                }
                Err(format!(
                    "wire schema {} has {} validation error(s)",
                    input.display(),
                    1
                ))
            }
        }
    }
}

fn read_text(path: &Path) -> Result<String, String> {
    let bytes = read_input(path)?;
    String::from_utf8(bytes).map_err(|e| format!("input is not UTF-8: {e}"))
}

fn read_input(path: &Path) -> Result<Vec<u8>, String> {
    if path.as_os_str() == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut buf)
            .map_err(|e| format!("read stdin: {}", e))?;
        Ok(buf)
    } else {
        fs::read(path).map_err(|e| format!("read {}: {}", path.display(), e))
    }
}

fn write_bytes(output: Option<&Path>, data: &[u8]) -> Result<(), String> {
    if let Some(path) = output {
        if path.as_os_str() == "-" {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(data)
                .map_err(|e| format!("write stdout: {}", e))
        } else {
            fs::write(path, data).map_err(|e| format!("write {}: {}", path.display(), e))
        }
    } else {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(data)
            .map_err(|e| format!("write stdout: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use clap::Parser;
    use std::path::PathBuf;

    fn test_dir(tag: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mimi_wire_test_{}_{}_{}",
            tag,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    #[test]
    fn wire_cli_encode_decode_roundtrip() {
        let dir = test_dir("roundtrip");
        let payload_path = dir.join("payload.bin");
        let wire_path = dir.join("message.wire");
        let decoded_path = dir.join("decoded.bin");

        let payload = b"wire-roundtrip\x00\x01\x02";
        std::fs::write(&payload_path, payload).expect("write payload");

        let action = WireAction::Encode {
            input: payload_path.clone(),
            output: Some(wire_path.clone()),
        };
        run(action).expect("encode should succeed");

        let encoded = std::fs::read(&wire_path).expect("read encoded");
        assert!(encoded.len() > payload.len());
        assert_eq!(encoded[..4], WireEnvelope::MAGIC.to_le_bytes());

        let action = WireAction::Decode {
            input: wire_path,
            output: Some(decoded_path.clone()),
        };
        run(action).expect("decode should succeed");

        let decoded = std::fs::read(&decoded_path).expect("read decoded");
        assert_eq!(decoded, payload);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wire_cli_decode_rejects_corrupt_envelope() {
        let dir = test_dir("corrupt");
        let bad_path = dir.join("bad.wire");
        std::fs::write(&bad_path, b"not-a-wire").expect("write bad wire");

        let action = WireAction::Decode {
            input: bad_path,
            output: None,
        };
        let err = run(action).expect_err("corrupt wire must fail");
        assert!(
            err.contains("invalid wire envelope"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wire_schema_validate_accepts_clean_schema() {
        let dir = test_dir("schema_ok");
        let path = dir.join("schema.json");
        std::fs::write(
            &path,
            r#"{"name":"Test","version":1,"fields":[{"name":"a","ty":"I32","index":0,"optional":false}]}"#,
        )
        .expect("write schema");

        run(WireAction::ValidateSchema {
            input: path.clone(),
        })
        .expect("clean schema should validate");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wire_schema_validate_rejects_noncontiguous_index() {
        let dir = test_dir("schema_bad");
        let path = dir.join("schema.json");
        std::fs::write(
            &path,
            r#"{"name":"Test","version":1,"fields":[{"name":"a","ty":"I32","index":0,"optional":false},{"name":"b","ty":"I64","index":2,"optional":false}]}"#,
        )
        .expect("write schema");

        let err = run(WireAction::ValidateSchema { input: path })
            .expect_err("non-contiguous schema must fail");
        assert!(err.contains("validation error"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wire_cli_subcommand_clap_parses() {
        let args = vec![
            "mimi",
            "wire",
            "encode",
            "--output",
            "/tmp/x.wire",
            "/tmp/x.bin",
        ];
        let parsed = crate::Args::parse_from(args);
        assert!(matches!(parsed.cmd, Command::Wire { .. }));
    }
}
