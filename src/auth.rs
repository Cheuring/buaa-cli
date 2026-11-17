use buaa_api::store::{cookies::CookieStore, cred::CredentialStore};
use buaa_api::{Context, ContextBuilder};
use std::path::Path;
use std::sync::Arc;

pub fn context(path: &Path, disable_tls: bool, interactive: bool) -> Context {
    let cookies = CookieStore::from_file(path.join("cookies.json")).unwrap_or_default();
    let cred = CredentialStore::from_file(path.join("cred.json")).unwrap_or_default();
    let builder = ContextBuilder::new()
        .cookies(cookies)
        .cred(cred)
        .tls(!disable_tls);
    if interactive {
        builder
            .captcha(Arc::new(crate::captcha::StdinSolver))
            .build()
    } else {
        builder
            .captcha(Arc::new(crate::captcha::BackgroundSolver))
            .build()
    }
}
