use std::path::PathBuf;

use clap::Args;

use crate::{config, deploy, state};

#[derive(Args)]
pub struct RekeyArgs {
    /// Path to the TOML definition file
    #[arg(short, long)]
    file: PathBuf,

    /// Path to the state file
    #[arg(long, default_value = "blue.state.json")]
    state: PathBuf,
}

pub fn run(args: &RekeyArgs) -> Result<(), Box<dyn std::error::Error>> {
    println!("Rekeying state: {}", args.state.display());

    let raw = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("failed to read {}: {e}", args.file.display()))?;
    let config_dir = match args.file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };
    let config = config::load(&raw, &std::collections::HashMap::new(), config_dir)
        .map_err(|e| format!("failed to parse {}: {e}", args.file.display()))?;

    if config.encryption.recipients.is_empty() {
        return Err("no encryption recipients configured in [encryption]".into());
    }

    // Load identity for decryption
    let identity_path = std::env::var("BLUE_AGE_IDENTITY")
        .map_err(|_| "BLUE_AGE_IDENTITY environment variable not set")?;
    let identities = deploy::load_identities(std::path::Path::new(&identity_path))?;

    // Load current state
    let mut current_state = state::load(&args.state)?;

    // Decrypt and re-encrypt all secrets in resource properties
    let mut rekeyed = 0;
    for (_name, snap) in current_state.resources.iter_mut() {
        rekey_value(&mut snap.properties, &identities, &config.encryption.recipients)?;
        rekey_value(&mut snap.outputs, &identities, &config.encryption.recipients)?;
        rekeyed += 1;
    }

    state::save(current_state, &args.state)?;
    println!("Rekeyed {rekeyed} resources. State saved to {}", args.state.display());
    Ok(())
}

fn rekey_value(
    value: &mut serde_json::Value,
    identities: &[Box<dyn age::Identity>],
    recipients: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use std::io::{Read, Write};

    match value {
        serde_json::Value::String(s) => {
            if let Some(rest) = s.strip_prefix("<encrypted:") {
                if let Some(rest) = rest.strip_suffix('>') {
                    if let Some((_old_hmac, b64)) = rest.split_once(':') {
                        // Decrypt
                        let ciphertext = base64::engine::general_purpose::STANDARD
                            .decode(b64)
                            .map_err(|e| format!("failed to decode: {e}"))?;

                        let identity_refs: Vec<&dyn age::Identity> = identities
                            .iter()
                            .map(|i| i.as_ref() as &dyn age::Identity)
                            .collect();

                        let decryptor = age::Decryptor::new(&ciphertext[..])
                            .map_err(|e| format!("failed to create decryptor: {e}"))?;
                        let mut reader = decryptor
                            .decrypt(identity_refs.into_iter())
                            .map_err(|e| format!("failed to decrypt: {e}"))?;
                        let mut plaintext = vec![];
                        reader.read_to_end(&mut plaintext)?;
                        let plaintext_str = String::from_utf8(plaintext)?;

                        // Recompute HMAC (use "rekey" as key since we don't know the original param name)
                        let mut mac = Hmac::<Sha256>::new_from_slice(b"rekey")
                            .expect("HMAC accepts any key size");
                        mac.update(plaintext_str.as_bytes());
                        let hmac_hex = hex::encode(mac.finalize().into_bytes());

                        // Re-encrypt with new recipients
                        let parsed_recipients: Vec<Box<dyn age::Recipient + Send>> = recipients
                            .iter()
                            .filter_map(|r| {
                                r.parse::<age::x25519::Recipient>()
                                    .ok()
                                    .map(|r| Box::new(r) as Box<dyn age::Recipient + Send>)
                            })
                            .collect();
                        let recipient_refs: Vec<&dyn age::Recipient> = parsed_recipients
                            .iter()
                            .map(|r| r.as_ref() as &dyn age::Recipient)
                            .collect();

                        let encryptor = age::Encryptor::with_recipients(recipient_refs.into_iter())
                            .map_err(|e| format!("failed to create encryptor: {e}"))?;
                        let mut encrypted = vec![];
                        let mut writer = encryptor.wrap_output(&mut encrypted)?;
                        writer.write_all(plaintext_str.as_bytes())?;
                        writer.finish()?;

                        let new_b64 = base64::engine::general_purpose::STANDARD.encode(&encrypted);
                        *s = format!("<encrypted:{hmac_hex}:{new_b64}>");
                    }
                }
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                rekey_value(v, identities, recipients)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                rekey_value(v, identities, recipients)?;
            }
        }
        _ => {}
    }
    Ok(())
}
