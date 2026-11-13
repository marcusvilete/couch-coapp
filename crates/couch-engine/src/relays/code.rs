//! Relay codes (M26a, ADR 0014): a relay's address and token on one line,
//! to hand to the people who may use it. Pasted in Settings → Network or
//! the wizard, never opened as a link (any web page can open a link).
//!
//! `couch-relay-1.<payload>.<check>`: the payload is base64url without
//! padding of three lines, `<url>\n<token>\n<flags>` (an empty token for a
//! relay open to everyone; `personal` or nothing for flags); the check is
//! the first 8 hex digits of the payload's SHA-256, so a code cut short or
//! changed in a chat is caught and said. Made the same way by the deploy
//! kit's `deploy/relay/relay-code.sh`, in plain bash.

use couch_net::{RelayUrl, endpoint::RelayToken};
use data_encoding::BASE64URL_NOPAD;
use sha2::{Digest, Sha256};

const PREFIX: &str = "couch-relay-";
const VERSION: &str = "1";
const PERSONAL: &str = "personal";
/// Longer than any code this app makes: a URL and a token fit many times.
const MAX_LEN: usize = 4096;

/// A relay's address and token, read from a code or about to become one.
#[derive(Clone, PartialEq, Eq)]
pub struct RelayCode {
    pub url: RelayUrl,
    pub token: Option<RelayToken>,
    /// Given to one person: never passed on (M26b), nor shared again.
    pub personal: bool,
}

impl std::fmt::Debug for RelayCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayCode")
            .field("url", &self.url.as_str())
            .field("token", &self.token)
            .field("personal", &self.personal)
            .finish()
    }
}

/// Why a pasted text isn't a code this app can use.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error("that isn't a relay code: relay codes start with couch-relay-1.")]
    NotACode,
    #[error("this relay code comes from a newer Couch Co-App: update this one first")]
    Newer,
    #[error("this relay code is incomplete or was changed: copy it again, whole")]
    Damaged,
    #[error("this relay code names no relay this app can use")]
    BadAddress,
}

impl RelayCode {
    /// The code, to share. Holds the token: never log it.
    pub fn encode(&self) -> String {
        let payload = payload(
            self.url.as_str(),
            self.token.as_ref().map_or("", RelayToken::reveal),
            self.personal,
        );
        format!(
            "{PREFIX}{VERSION}.{}.{}",
            BASE64URL_NOPAD.encode(payload.as_bytes()),
            check(payload.as_bytes())
        )
    }

    /// Reads a pasted code. Spaces and line breaks anywhere in it are
    /// ignored (a chat may wrap a long line).
    pub fn parse(text: &str) -> Result<Self, CodeError> {
        let code: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if code.len() > MAX_LEN {
            return Err(CodeError::Damaged);
        }
        let lower = code.to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix(PREFIX).map(|_| &code[PREFIX.len()..]) else {
            return Err(CodeError::NotACode);
        };
        let mut parts = rest.split('.');
        let (Some(version), Some(body), Some(sum), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return match rest.split('.').next() {
                Some(v) if v != VERSION && v.parse::<u32>().is_ok() => Err(CodeError::Newer),
                _ => Err(CodeError::Damaged),
            };
        };
        if version != VERSION {
            return Err(match version.parse::<u32>() {
                Ok(_) => CodeError::Newer,
                Err(_) => CodeError::Damaged,
            });
        }
        let payload = BASE64URL_NOPAD
            .decode(body.as_bytes())
            .map_err(|_| CodeError::Damaged)?;
        if !sum.eq_ignore_ascii_case(&check(&payload)) {
            return Err(CodeError::Damaged);
        }
        let payload = String::from_utf8(payload).map_err(|_| CodeError::Damaged)?;
        let mut lines = payload.split('\n');
        let (Some(url), Some(token), Some(flags), None) =
            (lines.next(), lines.next(), lines.next(), lines.next())
        else {
            return Err(CodeError::Damaged);
        };
        Ok(Self {
            url: url.trim().parse().map_err(|_| CodeError::BadAddress)?,
            token: RelayToken::new(token),
            personal: flags.split(',').any(|f| f.trim() == PERSONAL),
        })
    }
}

fn payload(url: &str, token: &str, personal: bool) -> String {
    format!("{url}\n{token}\n{}", if personal { PERSONAL } else { "" })
}

fn check(payload: &[u8]) -> String {
    Sha256::digest(payload)[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deploy kit makes this exact code from these values
    /// (`deploy/relay/relay-code.sh https://relay.example.com/ s3cret
    /// --personal`): keep both in step.
    const VECTOR: &str =
        "couch-relay-1.aHR0cHM6Ly9yZWxheS5leGFtcGxlLmNvbS8KczNjcmV0CnBlcnNvbmFs.441626e3";

    fn code(token: Option<&str>, personal: bool) -> RelayCode {
        RelayCode {
            url: "https://relay.example.com".parse().unwrap(),
            token: token.and_then(RelayToken::new),
            personal,
        }
    }

    #[test]
    fn a_code_reads_back_whole() {
        for original in [
            code(Some("s3cret"), false),
            code(Some("s3cret"), true),
            code(None, false),
        ] {
            assert_eq!(RelayCode::parse(&original.encode()).unwrap(), original);
        }
    }

    #[test]
    fn the_deploy_kit_s_code_reads() {
        let read = RelayCode::parse(VECTOR).unwrap();
        assert_eq!(read, code(Some("s3cret"), true));
        assert_eq!(read.encode(), VECTOR);
    }

    /// The deploy kit's script itself, where bash is there (Linux, Git
    /// Bash on Windows): its codes read, the vector's included.
    #[test]
    fn the_deploy_kit_s_script_makes_the_same_codes() {
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/relay/relay-code.sh"
        );
        let run = |args: &[&str]| {
            std::process::Command::new("bash")
                .arg(script)
                .args(args)
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8(out.stdout).unwrap().trim().to_string())
        };
        let Some(made) = run(&["https://relay.example.com/", "s3cret", "--personal"]) else {
            eprintln!("no bash here: the deploy kit's script wasn't run");
            return;
        };
        assert_eq!(made, VECTOR);
        let open = run(&["https://relay.example.com", "a token with spaces"]).unwrap();
        assert_eq!(
            RelayCode::parse(&open).unwrap(),
            RelayCode {
                url: "https://relay.example.com".parse().unwrap(),
                token: RelayToken::new("a token with spaces"),
                personal: false,
            }
        );
        assert!(run(&["relay.example.com", "t"]).is_none(), "not a URL");
        assert!(
            run(&["https://relay.example.com", ""]).is_none(),
            "no token"
        );
    }

    #[test]
    fn a_wrapped_or_padded_paste_still_reads() {
        let encoded = code(Some("s3cret"), false).encode();
        let (a, b) = encoded.split_at(20);
        let wrapped = format!("  {a}\r\n  {b} \n");
        assert_eq!(
            RelayCode::parse(&wrapped).unwrap(),
            code(Some("s3cret"), false)
        );
        assert!(
            RelayCode::parse(&encoded.to_uppercase()).is_err(),
            "base64 keeps its case"
        );
    }

    #[test]
    fn a_cut_or_changed_code_is_caught_and_said() {
        let encoded = code(Some("s3cret"), false).encode();
        assert_eq!(RelayCode::parse("hello"), Err(CodeError::NotACode));
        assert_eq!(
            RelayCode::parse("https://relay.example.com"),
            Err(CodeError::NotACode)
        );
        assert_eq!(
            RelayCode::parse(&encoded[..encoded.len() - 3]),
            Err(CodeError::Damaged)
        );
        let cut_payload = encoded.replacen(".aHR0", ".aHR", 1);
        assert_eq!(RelayCode::parse(&cut_payload), Err(CodeError::Damaged));
        // One character of the payload changed: the check catches it.
        let mut changed = encoded.clone().into_bytes();
        let at = "couch-relay-1.".len() + 10;
        changed[at] = if changed[at] == b'A' { b'B' } else { b'A' };
        assert_eq!(
            RelayCode::parse(std::str::from_utf8(&changed).unwrap()),
            Err(CodeError::Damaged)
        );
        assert_eq!(
            RelayCode::parse(&encoded.replacen("couch-relay-1.", "couch-relay-2.", 1)),
            Err(CodeError::Newer)
        );
        assert_eq!(
            RelayCode::parse("couch-relay-7.whatever"),
            Err(CodeError::Newer)
        );
    }

    #[test]
    fn a_code_naming_no_relay_is_refused() {
        let payload = payload("not a url", "t", false);
        let text = format!(
            "{PREFIX}{VERSION}.{}.{}",
            BASE64URL_NOPAD.encode(payload.as_bytes()),
            check(payload.as_bytes())
        );
        assert_eq!(RelayCode::parse(&text), Err(CodeError::BadAddress));
    }

    #[test]
    fn a_token_never_prints() {
        let code = code(Some("s3cret"), false);
        assert!(!format!("{code:?}").contains("s3cret"));
        assert!(!CodeError::Damaged.to_string().contains("s3cret"));
    }
}
