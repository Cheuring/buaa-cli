//! Short file locks protect snapshots/atomic writes, never network requests.

use buaa_api::store::{cookies::CookieStore, cred::CredentialStore};
use buaa_api::{Context, ContextBuilder};
use serde::de::DeserializeOwned;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(PartialEq, Eq)]
struct Snapshot {
    cookies: Option<Vec<u8>>,
    credentials: Option<Vec<u8>>,
}

impl Snapshot {
    fn read(path: &Path) -> io::Result<Self> {
        fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
            match fs::read(path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        }
        Ok(Self {
            cookies: read(&path.join("cookies.json"))?,
            credentials: read(&path.join("cred.json"))?,
        })
    }
}

fn lock(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.join("auth.lock"))?;
    file.lock()?;
    Ok(file)
}

pub struct Session {
    pub context: Context,
    path: PathBuf,
    snapshot: Snapshot,
}

impl Session {
    pub fn load(path: &Path, disable_tls: bool, interactive: bool) -> io::Result<Self> {
        let guard = lock(path)?;
        let snapshot = Snapshot::read(path)?;
        // CookieStore's file format is the inner domain map, whereas its Serde
        // representation has a `store` wrapper. Use its dedicated file reader.
        let cookies = match CookieStore::from_file(path.join("cookies.json")) {
            Ok(cookies) => cookies,
            Err(_) if interactive => {
                eprintln!("[Warning]: Failed to read cookies.json, use default");
                CookieStore::default()
            }
            Err(_) => {
                return Err(io::Error::other(
                    "Missing or invalid cookies.json; run `buaa login` interactively",
                ));
            }
        };
        drop(guard);
        fn parse<T: DeserializeOwned + Default>(
            bytes: &Option<Vec<u8>>,
            name: &str,
            interactive: bool,
        ) -> io::Result<T> {
            match bytes.as_deref().map(serde_json::from_slice).transpose() {
                Ok(Some(value)) => Ok(value),
                _ if interactive => {
                    eprintln!("[Warning]: Failed to read {name}, use default");
                    Ok(T::default())
                }
                _ => Err(io::Error::other(format!(
                    "Missing or invalid {name}; run `buaa login` interactively"
                ))),
            }
        }
        let cred: CredentialStore = parse(&snapshot.credentials, "cred.json", interactive)?;
        let builder = ContextBuilder::new()
            .cookies(cookies)
            .cred(cred)
            .tls(!disable_tls);
        let context = if interactive {
            builder
                .captcha(Arc::new(crate::captcha::StdinSolver))
                .build()
        } else {
            builder
                .captcha(Arc::new(crate::captcha::BackgroundSolver))
                .build()
        };
        Ok(Self {
            context,
            path: path.to_path_buf(),
            snapshot,
        })
    }

    /// An explicit login wins; older concurrent sessions must not overwrite it.
    pub fn save(&self, force: bool) -> io::Result<()> {
        let _guard = lock(&self.path)?;
        if force || Snapshot::read(&self.path)? == self.snapshot {
            let temporary = self.path.join("cookies.tmp");
            let mut options = OpenOptions::new();
            options.create(true).truncate(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(&temporary)?;
            self.context
                .get_cookies()
                .to_file(&temporary)
                .map_err(io::Error::other)?;
            file.sync_all()?;
            fs::rename(temporary, self.path.join("cookies.json"))?;
            crate::server::storage::write_json(
                &self.path.join("cred.json"),
                self.context.get_cred(),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buaa_api::store::cookies::Cookie;

    #[test]
    fn compatible_cookie_files_and_newer_login_wins() {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let path =
            std::env::temp_dir().join(format!("buaa-auth-test-{:x}", u64::from_ne_bytes(random)));
        fs::create_dir_all(&path).unwrap();
        let mut cookies = CookieStore::default();
        cookies.insert("example.invalid", Cookie::parse("test=fixture; Secure"));
        cookies.to_file(path.join("cookies.json")).unwrap();
        CredentialStore::default()
            .to_file(path.join("cred.json"))
            .unwrap();
        let session = Session::load(&path, false, false).unwrap();
        assert!(
            session
                .context
                .get_cookies()
                .get("example.invalid", "test")
                .is_some()
        );
        session.save(false).unwrap();
        let restored = CookieStore::from_file(path.join("cookies.json")).unwrap();
        assert!(restored.get("example.invalid", "test").is_some());
        let older = Session::load(&path, false, false).unwrap();
        let newer = Session::load(&path, false, false).unwrap();
        newer.context.set_username("new-login");
        newer.save(true).unwrap();
        let expected = Snapshot::read(&path).unwrap();
        older.context.set_username("stale-login");
        older.save(false).unwrap();
        assert!(Snapshot::read(&path).unwrap() == expected);
        fs::write(path.join("cookies.json"), "invalid").unwrap();
        assert!(Session::load(&path, false, false).is_err());
        assert_eq!(
            fs::read_to_string(path.join("cookies.json")).unwrap(),
            "invalid"
        );
        fs::remove_dir_all(path).unwrap();
    }
}
