//! One window for everything, when asked for: a second launch hands its paths to the first and
//! ends. The first listens on this machine only, and says where in a file in the settings folder,
//! with a secret to quote. So only someone able to read that folder can hand it anything, and what
//! they hand goes through the same checks as the command line: opening a folder asks the trust
//! question, and a link asks what links ask.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

const FILE: &str = "instance";
const MAX_MESSAGE: u64 = 64 * 1024;

fn where_from(dir: &Path) -> Option<(SocketAddr, String)> {
    let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
    let (port, secret) = text.trim().split_once(' ')?;
    Some((SocketAddr::from(([127, 0, 0, 1], port.parse().ok()?)), secret.to_string()))
}

/// A secret for the file. Not a cryptographic one: it stops a confused program on this machine
/// reaching the wrong thing, and the file's own permissions do the rest.
fn secret() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.write_u32(std::process::id());
    let a = h.finish();
    h.write_u64(a);
    format!("{a:016x}{:016x}", h.finish())
}

/// Tries to give these to a running copy. False means there's none, and this one should go ahead.
pub fn hand_off(dir: &Path, args: &[String]) -> bool {
    let Some((addr, secret)) = where_from(dir) else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    let mut message = format!("{secret}\n");
    for a in args {
        message.push_str(a);
        message.push('\n');
    }
    if s.write_all(message.as_bytes()).is_err() || s.shutdown(Shutdown::Write).is_err() {
        return false;
    }
    let mut answer = String::new();
    s.read_to_string(&mut answer).is_ok() && answer == "ok"
}

/// Listening. What arrives comes out of `incoming`, one launch at a time.
pub struct Instance {
    pub incoming: Receiver<Vec<String>>,
    file: PathBuf,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
    }
}

/// Starts listening. `wake` is called when something's arrived.
pub fn listen(dir: &Path, wake: impl Fn() + Send + 'static) -> std::io::Result<Instance> {
    std::fs::create_dir_all(dir)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let secret = secret();
    let file = dir.join(FILE);
    std::fs::write(&file, format!("{port} {secret}"))?;
    let (tx, incoming) = channel();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let Ok(reader) = s.try_clone() else { continue };
            let mut lines = BufReader::new(reader.take(MAX_MESSAGE)).lines().map_while(Result::ok);
            if lines.next().as_deref() != Some(secret.as_str()) {
                continue;
            }
            let args: Vec<String> = lines.collect();
            if tx.send(args).is_err() {
                return;
            }
            let mut s = s;
            let _ = s.write_all(b"ok");
            wake();
        }
    });
    Ok(Instance { incoming, file })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(i: &Instance) -> Vec<String> {
        i.incoming.recv_timeout(Duration::from_secs(10)).expect("what was handed over")
    }

    #[test]
    fn a_second_launch_hands_over_what_it_was_asked_for() {
        let d = tempfile::tempdir().unwrap();
        let i = listen(d.path(), || {}).unwrap();
        let args = vec!["C:/some folder/repo".to_string(), "gitem://open?repo=x".to_string()];
        assert!(hand_off(d.path(), &args));
        assert_eq!(wait(&i), args);
        assert!(hand_off(d.path(), &[]), "with nothing to open, it still means come to the front");
        assert!(wait(&i).is_empty());
    }

    #[test]
    fn nothing_is_there_and_the_launch_goes_ahead() {
        let d = tempfile::tempdir().unwrap();
        assert!(!hand_off(d.path(), &[]), "no file");
        std::fs::write(d.path().join(FILE), "1 stale").unwrap();
        assert!(!hand_off(d.path(), &[]), "a file from one that's gone");
        std::fs::write(d.path().join(FILE), "rubbish").unwrap();
        assert!(!hand_off(d.path(), &[]), "a file that doesn't read");
    }

    #[test]
    fn the_wrong_secret_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        let i = listen(d.path(), || {}).unwrap();
        let (addr, _) = where_from(d.path()).unwrap();
        std::fs::write(d.path().join(FILE), format!("{} wrong", addr.port())).unwrap();
        assert!(!hand_off(d.path(), &["x".to_string()]));
        assert!(i.incoming.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn closing_takes_the_file_with_it() {
        let d = tempfile::tempdir().unwrap();
        let i = listen(d.path(), || {}).unwrap();
        assert!(d.path().join(FILE).exists());
        drop(i);
        assert!(!d.path().join(FILE).exists());
    }
}
