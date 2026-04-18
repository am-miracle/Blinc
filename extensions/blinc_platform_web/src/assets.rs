//! Browser asset loader
//!
//! Browsers can't block the main thread, so the standard
//! [`AssetLoader`] trait — which has a synchronous `load()` method —
//! is satisfied via a pre-fetched in-memory cache:
//!
//! 1. The app calls [`WebAssetLoader::preload`] from an `async`
//!    bootstrap function. That walks a list of asset URLs, fetches
//!    each one with the browser `fetch()` API, and stuffs the bytes
//!    into the loader's cache.
//! 2. After preload completes, the synchronous `AssetLoader::load`
//!    contract is satisfied by reading from the cache.
//!
//! This is the same model Android uses with APK assets — bytes are
//! "available" once the runtime hands you the resource manager,
//! never read lazily through a blocking I/O call.

use std::collections::HashMap;
use std::sync::Mutex;

use blinc_platform::assets::{AssetLoader, AssetPath};
use blinc_platform::{PlatformError, Result};

/// In-memory asset loader for the web target.
///
/// All loaded bytes live in a `Mutex<HashMap>` so the loader can
/// satisfy the `AssetLoader: Send + Sync` bound. Lookups are cheap
/// (single hash + clone of the byte vector). The cache is unbounded
/// — preloaded assets stay resident for the lifetime of the loader.
/// Shared snapshot of a preload pass's progress. Bumped atomically as
/// each URL's `fetch()` resolves. Apps clone the `Arc` returned by
/// [`WebAssetLoader::progress`] once and query it every frame to drive
/// a loading-state UI (spinner, progress bar, asset name readout).
///
/// Counter semantics:
/// - `total`: number of URLs the *current* preload pass was asked to
///   fetch. Increases by `urls.len()` when `preload()` starts.
/// - `completed`: fetches that resolved successfully and were inserted
///   into the cache.
/// - `failed`: fetches that returned an error (404, network drop,
///   decode error in the platform's `fetch_bytes`).
///
/// Call [`Self::is_complete`] to check "all URLs accounted for". A
/// pass with zero URLs is considered complete immediately.
#[derive(Debug, Default)]
pub struct PreloadProgress {
    total: std::sync::atomic::AtomicUsize,
    completed: std::sync::atomic::AtomicUsize,
    failed: std::sync::atomic::AtomicUsize,
}

impl PreloadProgress {
    pub fn total(&self) -> usize {
        self.total.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn completed(&self) -> usize {
        self.completed.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn failed(&self) -> usize {
        self.failed.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Number of fetches that have either succeeded or failed — i.e.
    /// ones that are no longer in flight.
    pub fn settled(&self) -> usize {
        self.completed() + self.failed()
    }
    /// `true` iff every requested fetch has resolved (success or fail).
    /// A pass that hasn't started yet (no URLs registered) is also
    /// complete by this definition.
    pub fn is_complete(&self) -> bool {
        let t = self.total();
        t == 0 || self.settled() >= t
    }
    /// Progress as a `0..=1` fraction. Returns `1.0` before any URL
    /// has been registered.
    pub fn fraction(&self) -> f32 {
        let t = self.total();
        if t == 0 {
            1.0
        } else {
            (self.settled() as f32 / t as f32).min(1.0)
        }
    }
}

#[derive(Debug, Default)]
pub struct WebAssetLoader {
    cache: Mutex<HashMap<String, Vec<u8>>>,
    progress: std::sync::Arc<PreloadProgress>,
}

impl WebAssetLoader {
    /// Create an empty loader. Use [`preload`](Self::preload) to fill
    /// it before any synchronous `load()` call.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clone the shared preload-progress handle. Cheap (Arc bump). The
    /// returned value reflects the loader's live state — polling it
    /// on subsequent frames shows real-time progress.
    ///
    /// Typical pattern:
    ///
    /// ```ignore
    /// let progress = app.asset_loader().progress();  // one clone
    /// build_ui(move |ctx| {
    ///     // read each frame via the Arc — no borrow of the loader
    ///     let p = progress.clone();
    ///     div().child(text(format!("{:.0}%", p.fraction() * 100.0)))
    /// });
    /// ```
    pub fn progress(&self) -> std::sync::Arc<PreloadProgress> {
        self.progress.clone()
    }

    /// Insert raw bytes for `key` directly into the cache. Useful when
    /// the app already has asset bytes in hand (e.g. via
    /// `include_bytes!` for tiny bundled fonts) and doesn't need a
    /// `fetch()` round-trip.
    pub fn insert_raw(&self, key: impl Into<String>, bytes: Vec<u8>) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key.into(), bytes);
        }
    }

    /// Number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pre-load `urls` into the cache via the browser `fetch()` API.
    ///
    /// Fetches are issued in parallel — the browser's HTTP connection
    /// pool handles the fan-out (typically 6 concurrent requests per
    /// origin). A previous serial implementation turned a 1–2 s
    /// pipelined download of an 11-file / ~74 MB glTF asset into a
    /// 5–10 s wall-time stall on the event loop, because each
    /// `fetch_bytes().await` had to complete before the next started.
    ///
    /// Each URL is stored under its own string as the cache key, so
    /// `loader.load(AssetPath::Relative("fonts/Inter.ttf".into()))`
    /// resolves to the bytes fetched from `"fonts/Inter.ttf"`.
    ///
    /// On non-wasm hosts this returns immediately with success but
    /// does nothing — there's nothing to fetch from outside a browser.
    #[cfg(target_arch = "wasm32")]
    pub async fn preload(&self, urls: &[&str]) -> Result<()> {
        use futures::future::join_all;
        use std::sync::atomic::Ordering;

        // Bump the total *before* we start awaiting anything so UI
        // polling sees "N assets queued" on the very first frame after
        // preload kicks off. Otherwise the first frame would read
        // 0/0 (fraction = 1.0 = "done") and flash past the loading
        // state.
        self.progress.total.fetch_add(urls.len(), Ordering::Release);

        let fetches = urls.iter().map(|u| {
            let progress = self.progress.clone();
            async move {
                let result = Self::fetch_bytes(u).await.map(|b| (*u, b));
                match &result {
                    Ok(_) => progress.completed.fetch_add(1, Ordering::Release),
                    Err(_) => progress.failed.fetch_add(1, Ordering::Release),
                };
                result
            }
        });
        let results = join_all(fetches).await;

        // Partial-failure tolerance: populate the cache with every
        // successful fetch *before* reporting an error for the ones
        // that didn't land. Previously a single 404 (or flaky
        // connection) early-returned via `?` and discarded every
        // already-completed fetch — the caller would then see every
        // subsequent `load_asset` fail with "not preloaded" because
        // the cache was empty, even though most of the bytes did
        // arrive. For a 29-asset scene (e.g. the strangler rig),
        // one missing texture shouldn't mean nothing renders.
        //
        // Fatal vs. non-fatal: if *nothing* loaded we still bubble
        // the error (definitely broken — e.g. wrong base URL). If
        // some fraction landed, log at `warn` and return Ok so the
        // caller continues; missing textures fall back to the
        // renderer's 1×1 default and the rest of the scene renders.
        let mut first_err: Option<PlatformError> = None;
        let mut inserted = 0usize;
        for result in results {
            match result {
                Ok((url, bytes)) => {
                    self.insert_raw(url, bytes);
                    inserted += 1;
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        match first_err {
            Some(e) if inserted == 0 => Err(e),
            Some(e) => {
                tracing::warn!(
                    "preload partially failed: {} of {} succeeded ({}): {e}",
                    inserted,
                    urls.len(),
                    urls.len() - inserted,
                );
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Cross-host placeholder for `preload`. See the wasm32 variant
    /// for the real implementation.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn preload(&self, _urls: &[&str]) -> Result<()> {
        Ok(())
    }

    /// Fetch a single URL and return its bytes, without inserting
    /// into the cache.
    ///
    /// Useful when the caller wants to forward bytes directly to
    /// another consumer (e.g. `WebApp::load_font_data` for the
    /// font registry) without keeping a copy in this loader's
    /// HashMap. The cache-keyed [`Self::preload`] is the right
    /// choice when the same URL might be requested again later
    /// via [`AssetLoader::load`]; `fetch_bytes` is the right
    /// choice for one-shot bytes that have a downstream owner.
    #[cfg(target_arch = "wasm32")]
    pub async fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
        fetch_as_bytes(url).await
    }

    /// Cross-host placeholder for `fetch_bytes`. Returns
    /// `PlatformError::Unsupported` because there is no `fetch()`
    /// API outside a browser. The non-wasm32 path exists so
    /// downstream `cargo check` from a desktop box doesn't error
    /// on the missing item.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn fetch_bytes(_url: &str) -> Result<Vec<u8>> {
        Err(PlatformError::Unsupported(
            "WebAssetLoader::fetch_bytes is wasm32-only".to_string(),
        ))
    }

    fn key_for(path: &AssetPath) -> String {
        match path {
            AssetPath::Relative(rel) => rel.clone(),
            AssetPath::Absolute(abs) => abs.clone(),
            AssetPath::Embedded(name) => name.to_string(),
        }
    }
}

/// Newtype wrapper around `Arc<WebAssetLoader>` that implements
/// `AssetLoader`. Needed so one `Arc` clone can be registered via
/// `set_global_asset_loader(Box::new(SharedWebAssetLoader(…)))` while
/// another clone is kept for `insert_raw` / `preload`.
#[derive(Clone)]
pub struct SharedWebAssetLoader(pub std::sync::Arc<WebAssetLoader>);

impl AssetLoader for SharedWebAssetLoader {
    fn load(&self, path: &AssetPath) -> Result<Vec<u8>> {
        self.0.load(path)
    }
    fn exists(&self, path: &AssetPath) -> bool {
        self.0.exists(path)
    }
    fn platform_name(&self) -> &'static str {
        "web"
    }
}

impl AssetLoader for WebAssetLoader {
    fn load(&self, path: &AssetPath) -> Result<Vec<u8>> {
        let key = Self::key_for(path);
        let cache = self
            .cache
            .lock()
            .map_err(|e| PlatformError::AssetLoad(format!("WebAssetLoader cache poisoned: {e}")))?;
        cache.get(&key).cloned().ok_or_else(|| {
            PlatformError::AssetLoad(format!(
                "Asset '{key}' not preloaded — call WebAssetLoader::preload before run"
            ))
        })
    }

    fn exists(&self, path: &AssetPath) -> bool {
        let key = Self::key_for(path);
        self.cache
            .lock()
            .map(|cache| cache.contains_key(&key))
            .unwrap_or(false)
    }

    fn platform_name(&self) -> &'static str {
        "web"
    }
}

// =============================================================================
// fetch() bridge — wasm32 only
// =============================================================================

#[cfg(target_arch = "wasm32")]
async fn fetch_as_bytes(url: &str) -> Result<Vec<u8>> {
    use js_sys::Uint8Array;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{Request, RequestInit, RequestMode, Response};

    let opts = RequestInit::new();
    opts.set_method("GET");
    opts.set_mode(RequestMode::Cors);

    let request = Request::new_with_str_and_init(url, &opts).map_err(|e| {
        PlatformError::AssetLoad(format!("Failed to build request for {url}: {e:?}"))
    })?;

    let window = web_sys::window()
        .ok_or_else(|| PlatformError::AssetLoad("No global window object".to_string()))?;

    let resp_val = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| PlatformError::AssetLoad(format!("fetch({url}) failed: {e:?}")))?;
    let response: Response = resp_val
        .dyn_into()
        .map_err(|_| PlatformError::AssetLoad(format!("fetch({url}) returned non-Response")))?;
    if !response.ok() {
        return Err(PlatformError::AssetLoad(format!(
            "fetch({url}) returned HTTP {}",
            response.status()
        )));
    }
    let buf_val = JsFuture::from(
        response
            .array_buffer()
            .map_err(|e| PlatformError::AssetLoad(format!("array_buffer() error: {e:?}")))?,
    )
    .await
    .map_err(|e| PlatformError::AssetLoad(format!("array_buffer() rejected: {e:?}")))?;
    let array = Uint8Array::new(&buf_val);
    let mut bytes = vec![0u8; array.length() as usize];
    array.copy_to(&mut bytes);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_asset_returns_error() {
        let loader = WebAssetLoader::new();
        let result = loader.load(&AssetPath::Relative("nope.ttf".into()));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(format!("{err}").contains("nope.ttf"));
    }

    #[test]
    fn insert_then_load_round_trips() {
        let loader = WebAssetLoader::new();
        loader.insert_raw("logo.png", vec![1, 2, 3, 4]);
        let bytes = loader
            .load(&AssetPath::Relative("logo.png".into()))
            .expect("preloaded asset should be present");
        assert_eq!(bytes, vec![1, 2, 3, 4]);
        assert!(loader.exists(&AssetPath::Relative("logo.png".into())));
        assert!(!loader.exists(&AssetPath::Relative("missing.png".into())));
    }

    #[test]
    fn embedded_paths_match_relative_lookup() {
        let loader = WebAssetLoader::new();
        loader.insert_raw("hero.svg", vec![42]);
        // Embedded and relative paths produce the same cache key, so
        // the same bytes come back regardless of which form the user
        // calls with.
        assert_eq!(
            loader.load(&AssetPath::Embedded("hero.svg")).unwrap(),
            vec![42]
        );
    }

    #[test]
    fn platform_name_is_web() {
        let loader = WebAssetLoader::new();
        assert_eq!(loader.platform_name(), "web");
    }

    #[test]
    fn empty_loader_reports_empty() {
        let loader = WebAssetLoader::new();
        assert_eq!(loader.len(), 0);
        assert!(loader.is_empty());
    }
}
