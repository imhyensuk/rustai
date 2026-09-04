//! Conditional headless-Chrome fallback.
//!
//! This exists for the minority of pages that ship an empty shell and build the
//! document in JavaScript. It is deliberately awkward to reach: a render costs
//! roughly a hundred fetches in both time and memory, so [`super::needs_javascript`]
//! has to say yes first.
//!
//! One browser process is shared for the life of the program and started on
//! first use, because process startup — not navigation — is what actually
//! costs. Call [`shutdown`] to stop it early.

use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use futures_util::StreamExt;
use tokio::sync::{Mutex, OnceCell};
use tokio::task::JoinHandle;

use crate::error::{Error, Result};

struct Instance {
    browser: Mutex<Option<Browser>>,
    pump: Mutex<Option<JoinHandle<()>>>,
}

static INSTANCE: OnceCell<Arc<Instance>> = OnceCell::const_new();

async fn instance(launch_timeout: Duration) -> Result<Arc<Instance>> {
    INSTANCE
        .get_or_try_init(|| async {
            let config = BrowserConfig::builder()
                .no_sandbox()
                .new_headless_mode()
                .launch_timeout(launch_timeout)
                .request_timeout(launch_timeout)
                .viewport(None)
                .args([
                    "--disable-gpu",
                    "--disable-dev-shm-usage",
                    "--disable-background-networking",
                    "--disable-extensions",
                    "--blink-settings=imagesEnabled=false",
                    "--mute-audio",
                ])
                .build()
                .map_err(Error::Browser)?;

            let (browser, mut handler) =
                Browser::launch(config).await.map_err(|e| Error::Browser(e.to_string()))?;

            // The handler stream *is* the CDP connection: nothing works unless
            // something keeps polling it.
            let pump = tokio::spawn(async move { while handler.next().await.is_some() {} });

            Ok::<_, Error>(Arc::new(Instance {
                browser: Mutex::new(Some(browser)),
                pump: Mutex::new(Some(pump)),
            }))
        })
        .await
        .cloned()
}

/// Render `url` and return the DOM after scripts have run.
pub async fn render(url: &str, timeout: Duration) -> Result<String> {
    let inst = instance(timeout).await?;
    let page = {
        let guard = inst.browser.lock().await;
        let browser =
            guard.as_ref().ok_or_else(|| Error::Browser("browser is shut down".into()))?;
        browser.new_page(url).await.map_err(|e| Error::Browser(e.to_string()))?
    };

    let html = tokio::time::timeout(timeout, async {
        page.wait_for_navigation().await.map_err(|e| Error::Browser(e.to_string()))?;
        // Navigation completing is not the same as the app having rendered;
        // a short settle beats a fragile selector wait on an unknown page.
        tokio::time::sleep(Duration::from_millis(600)).await;
        page.content().await.map_err(|e| Error::Browser(e.to_string()))
    })
    .await
    .map_err(|_| Error::Browser(format!("render timed out after {timeout:?}")))?;

    let _ = page.close().await;
    html
}

/// Stop the shared browser, if one was started.
pub async fn shutdown() {
    let Some(inst) = INSTANCE.get() else { return };
    if let Some(mut browser) = inst.browser.lock().await.take() {
        let _ = browser.close().await;
        let _ = browser.wait().await;
    }
    if let Some(pump) = inst.pump.lock().await.take() {
        pump.abort();
    }
}
