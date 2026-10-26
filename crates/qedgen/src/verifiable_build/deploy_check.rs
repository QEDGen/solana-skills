//! Compare a deployed program with the verified build (#449). A mismatch
//! is a finding. It never changes the stamp, the evidence or the record:
//! those stay tied to the commit we verified.

use anyhow::Result;

use super::{selfcheck, BuildRecord};
use crate::upstream_check::BinaryFetcher;

#[derive(Debug, PartialEq, Eq)]
pub enum DeployCheck {
    Match {
        program_hash: String,
    },
    Mismatch {
        on_chain: String,
        recorded: String,
        commit: String,
    },
    NotDeployed {
        program_id: String,
    },
    NoRecord,
}

/// Fetch `program_id` and compare its hash, computed the `solana-verify`
/// way, with the build record. A fetch error that says the account does not
/// exist is `NotDeployed`; any other fetch error is returned, not hidden.
pub fn check_deployed(
    record: Option<&BuildRecord>,
    program_id: &str,
    fetcher: &mut dyn BinaryFetcher,
) -> Result<DeployCheck> {
    let Some(record) = record else {
        return Ok(DeployCheck::NoRecord);
    };
    let bytes = match fetcher.fetch(program_id) {
        Ok(bytes) => bytes,
        Err(e) => {
            if is_missing_account(&format!("{e:#}")) {
                return Ok(DeployCheck::NotDeployed {
                    program_id: program_id.to_string(),
                });
            }
            return Err(e);
        }
    };
    let on_chain = selfcheck::program_hash(&bytes);
    if on_chain == record.program_hash {
        Ok(DeployCheck::Match {
            program_hash: on_chain,
        })
    } else {
        Ok(DeployCheck::Mismatch {
            on_chain,
            recorded: record.program_hash.clone(),
            commit: record.commit.clone(),
        })
    }
}

/// True when a `solana program dump` error says the account does not exist.
/// The CLI also prints `AccountNotFound` when it cannot reach the RPC at
/// all, so a message that mentions a failed request is never "not deployed".
fn is_missing_account(msg: &str) -> bool {
    if msg.contains("error sending request") || msg.contains("Connection refused") {
        return false;
    }
    msg.contains("Unable to find the account") || msg.contains("AccountNotFound")
}

pub fn render(check: &DeployCheck, program_id: &str) -> String {
    match check {
        DeployCheck::Match { program_hash } => {
            format!("deployed program {program_id} matches the verified build ({program_hash})")
        }
        DeployCheck::Mismatch {
            on_chain,
            recorded,
            commit,
        } => format!(
            "P2: deployed program {program_id} hash {on_chain} differs from the verified build \
             {recorded} at commit {commit}; the stamp still covers that commit"
        ),
        DeployCheck::NotDeployed { program_id } => {
            format!("program {program_id} is not deployed on this cluster")
        }
        DeployCheck::NoRecord => format!(
            "no qedgen build record to compare {program_id} against; run qedgen build first"
        ),
    }
}

pub fn print_deploy_check(check: &DeployCheck, program_id: &str) {
    eprintln!("{}", render(check, program_id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream_check::BinaryFetcher;
    use crate::verifiable_build::{selfcheck::program_hash, BuildRecord};

    struct Fake {
        reply: std::result::Result<Vec<u8>, String>,
        calls: usize,
    }

    impl BinaryFetcher for Fake {
        fn fetch(&mut self, _program_id: &str) -> anyhow::Result<Vec<u8>> {
            self.calls += 1;
            match &self.reply {
                Ok(bytes) => Ok(bytes.clone()),
                Err(e) => anyhow::bail!("{e}"),
            }
        }
    }

    fn record_for(bytes: &[u8]) -> BuildRecord {
        BuildRecord {
            schema_version: 1,
            commit: "a".repeat(40),
            library: "p".into(),
            program_hash: program_hash(bytes),
            file_sha256: String::new(),
            solana_verify_version: "0.5.2".into(),
            image: None,
            platform_tools: "v1.57".into(),
            arch: "v3".into(),
            debug_setting: "1".into(),
            build_argv: vec![],
            workspace_path: String::new(),
            text_matches: true,
            recorded_at_unix: 1,
        }
    }

    #[test]
    fn padded_on_chain_bytes_match() {
        let r = record_for(b"elf");
        let mut f = Fake {
            reply: Ok(b"elf\0\0\0\0".to_vec()),
            calls: 0,
        };
        assert_eq!(
            check_deployed(Some(&r), "Prog", &mut f).unwrap(),
            DeployCheck::Match {
                program_hash: program_hash(b"elf")
            }
        );
    }

    #[test]
    fn different_bytes_mismatch() {
        let r = record_for(b"elf");
        let mut f = Fake {
            reply: Ok(b"other".to_vec()),
            calls: 0,
        };
        assert_eq!(
            check_deployed(Some(&r), "Prog", &mut f).unwrap(),
            DeployCheck::Mismatch {
                on_chain: program_hash(b"other"),
                recorded: program_hash(b"elf"),
                commit: "a".repeat(40),
            }
        );
    }

    /// The message `solana program dump` (solana-cli 4.3.0) prints for an
    /// account that does not exist on the cluster.
    #[test]
    fn missing_account_is_not_deployed() {
        let r = record_for(b"elf");
        let mut f = Fake {
            reply: Err(
                "`solana program dump Prog` failed: Error: Unable to find the account Prog".into(),
            ),
            calls: 0,
        };
        assert_eq!(
            check_deployed(Some(&r), "Prog", &mut f).unwrap(),
            DeployCheck::NotDeployed {
                program_id: "Prog".into()
            }
        );
    }

    /// With no RPC reachable the CLI also says `AccountNotFound`, but that is
    /// a network failure, not a missing program.
    #[test]
    fn unreachable_rpc_is_an_error_not_not_deployed() {
        let r = record_for(b"elf");
        let mut f = Fake {
            reply: Err(
                "`solana program dump Prog` failed: Error: AccountNotFound: pubkey=Prog: \
                        error sending request for url (http://127.0.0.1:8899/)"
                    .into(),
            ),
            calls: 0,
        };
        assert!(check_deployed(Some(&r), "Prog", &mut f).is_err());
    }

    #[test]
    fn other_fetch_errors_are_errors() {
        let r = record_for(b"elf");
        let mut f = Fake {
            reply: Err("connection refused".into()),
            calls: 0,
        };
        assert!(check_deployed(Some(&r), "Prog", &mut f).is_err());
    }

    #[test]
    fn no_record_reported() {
        let mut f = Fake {
            reply: Ok(vec![]),
            calls: 0,
        };
        assert_eq!(
            check_deployed(None, "Prog", &mut f).unwrap(),
            DeployCheck::NoRecord
        );
        assert_eq!(f.calls, 0);
    }

    #[test]
    fn mismatch_message_is_a_p2_that_keeps_the_stamp() {
        let msg = render(
            &DeployCheck::Mismatch {
                on_chain: "x".into(),
                recorded: "y".into(),
                commit: "c".into(),
            },
            "Prog",
        );
        assert!(
            msg.starts_with("P2: deployed program Prog hash x differs"),
            "{msg}"
        );
        assert!(msg.contains("the stamp still covers that commit"), "{msg}");
    }
}
