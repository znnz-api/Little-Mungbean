//! macOS-only PAC cache. Observe network state without a background run loop.
use super::*;
use std::cell::RefCell;

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCreate(allocator: Ref, name: Ref, callout: Ref, context: Ref) -> Ref;
    fn SCDynamicStoreCopyMultiple(store: Ref, keys: Ref, patterns: Ref) -> Ref;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(data: Ref) -> Index;
    fn CFDataGetBytePtr(data: Ref) -> *const u8;
    fn CFArrayCreate(allocator: Ref, values: *const Ref, count: Index, callbacks: Ref) -> Ref;
    fn CFDictionaryGetCount(dictionary: Ref) -> Index;
    fn CFDictionaryGetKeysAndValues(dictionary: Ref, keys: *mut Ref, values: *mut Ref);
}

// Compare property-list values, not dictionary iteration/serialization order.
// Unknown value types disable caching rather than hiding a network change.
fn snapshot_value(value: Ref, depth: usize) -> Option<serde_json::Value> {
    if value.is_null() || depth > 12 {
        return None;
    }
    let kind = unsafe { CFGetTypeID(value) };
    if kind == unsafe { CFStringGetTypeID() } {
        return string(value).map(serde_json::Value::String);
    }
    if kind == unsafe { CFBooleanGetTypeID() } {
        return Some(serde_json::Value::Bool(
            unsafe { CFBooleanGetValue(value) } != 0,
        ));
    }
    if kind == unsafe { CFNumberGetTypeID() } {
        let mut result = 0i64;
        return (unsafe { CFNumberGetValue(value, 4, (&mut result as *mut i64).cast()) } != 0)
            .then(|| serde_json::Value::from(result));
    }
    if kind == unsafe { CFDataGetTypeID() } {
        let length = unsafe { CFDataGetLength(value) };
        let bytes = unsafe { CFDataGetBytePtr(value) };
        if !(0..=65_536).contains(&length) || length > 0 && bytes.is_null() {
            return None;
        }
        if length == 0 {
            return Some(serde_json::Value::Array(Vec::new()));
        }
        let bytes = unsafe { std::slice::from_raw_parts(bytes, length as usize) };
        return Some(serde_json::Value::Array(
            bytes
                .iter()
                .map(|byte| serde_json::Value::from(*byte))
                .collect(),
        ));
    }
    if kind == unsafe { CFArrayGetTypeID() } {
        let count = unsafe { CFArrayGetCount(value) };
        if !(0..=4096).contains(&count) {
            return None;
        }
        return (0..count)
            .map(|index| snapshot_value(unsafe { CFArrayGetValueAtIndex(value, index) }, depth + 1))
            .collect::<Option<Vec<_>>>()
            .map(serde_json::Value::Array);
    }
    if kind == unsafe { CFDictionaryGetTypeID() } {
        let count = unsafe { CFDictionaryGetCount(value) };
        if !(0..=4096).contains(&count) {
            return None;
        }
        let mut keys = vec![std::ptr::null(); count as usize];
        let mut values = vec![std::ptr::null(); count as usize];
        unsafe { CFDictionaryGetKeysAndValues(value, keys.as_mut_ptr(), values.as_mut_ptr()) };
        return keys
            .into_iter()
            .zip(values)
            .map(|(key, value)| Some((string(key)?, snapshot_value(value, depth + 1)?)))
            .collect::<Option<serde_json::Map<_, _>>>()
            .map(serde_json::Value::Object);
    }
    None
}

fn network_snapshot() -> Option<serde_json::Value> {
    let pattern = cf_string(
        "State:/Network/(Global|Service/[^/]+|Interface/[^/]+)/(IPv4|IPv6|DNS|Proxies|Link|PPP|IPSec)$",
    ).ok()?;
    // The borrowed string stays alive until CopyMultiple has consumed the array.
    let patterns =
        Owned::new(unsafe { CFArrayCreate(std::ptr::null(), &pattern.0, 1, std::ptr::null()) })
            .ok()?;
    let store = STORE.with(|store| {
        let mut store = store.borrow_mut();
        if store.is_none() {
            let name = cf_string("Little-Mungbean.PAC.NetworkSnapshot").ok()?;
            *store = Some(
                Owned::new(unsafe {
                    SCDynamicStoreCreate(
                        std::ptr::null(),
                        name.0,
                        std::ptr::null(),
                        std::ptr::null(),
                    )
                })
                .ok()?,
            );
        }
        Some(store.as_ref()?.0)
    })?;
    let state =
        Owned::new(unsafe { SCDynamicStoreCopyMultiple(store, std::ptr::null(), patterns.0) })
            .ok()?;
    snapshot_value(state.0, 0)
}

thread_local! {
    static STORE: RefCell<Option<Owned>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct AutoCache {
    network: Option<serde_json::Value>,
    generation: u64,
    entries: HashMap<String, CachedAuto>,
}

impl AutoCache {
    fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.entries.clear();
    }

    fn observe(&mut self, network: Option<serde_json::Value>) {
        if network.is_none() || self.network != network {
            self.clear();
            self.network = network;
        }
    }

    fn get(&self, url: &Url, settings: &SystemSettings) -> Option<Vec<Route>> {
        let entry = self.entries.get(url.as_str())?;
        (self.network.is_some()
            && entry.settings == *settings
            && entry.resolved_at.elapsed() < PAC_CACHE_TTL)
            .then(|| entry.routes.clone())
    }

    fn insert(&mut self, url: &Url, settings: &SystemSettings, routes: &[Route], generation: u64) {
        // A manual refresh or another resolver may invalidate us while CFNetwork
        // is running. Never put those stale results back into the cache.
        if self.network.is_none() || self.generation != generation {
            return;
        }
        if self.entries.len() >= 128 {
            self.entries.clear();
        }
        self.entries.insert(
            url.as_str().to_owned(),
            CachedAuto {
                settings: settings.clone(),
                resolved_at: Instant::now(),
                routes: routes.to_vec(),
            },
        );
    }
}

static CACHE: OnceLock<Mutex<AutoCache>> = OnceLock::new();

fn cache() -> &'static Mutex<AutoCache> {
    CACHE.get_or_init(Default::default)
}

pub(super) fn refresh() {
    if let Ok(mut cache) = cache().lock() {
        cache.clear();
    }
}

pub(super) fn resolve_cached(url: &Url, settings: &SystemSettings) -> Result<Vec<Route>> {
    let network = network_snapshot();
    // A successful snapshot lets the cache tell whether the network changed.
    // If another network appears while PAC executes, discard its result and
    // resolve once again under the new network before opening a connection.
    let generation = if let Ok(mut cache) = cache().lock() {
        cache.observe(network.clone());
        if let Some(routes) = cache.get(url, settings) {
            return Ok(routes);
        }
        Some(cache.generation)
    } else {
        None
    };
    let routes = resolve_system_auto(url, settings)
        .context("自动代理解析失败 / Automatic proxy resolution failed")?;
    let after = network_snapshot();
    if network != after {
        if let Ok(mut cache) = cache().lock() {
            cache.observe(after);
        }
        return resolve_system_auto(url, settings)
            .context("网络变化后自动代理重新解析失败 / Automatic proxy re-resolution failed after network change");
    }
    if let Some(generation) = generation
        && let Ok(mut cache) = cache().lock()
    {
        cache.insert(url, settings, &routes, generation);
    }
    Ok(routes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_and_manual_refresh_reject_cached_and_in_flight_routes() {
        let url = Url::parse("https://model.example/v1/models").unwrap();
        let settings = SystemSettings::default();
        let first = serde_json::json!({"interface": "en0", "dns": ["192.0.2.1"]});
        let second = serde_json::json!({"interface": "utun1", "dns": ["192.0.2.2"]});
        let mut cache = AutoCache::default();
        cache.observe(Some(first.clone()));
        let generation = cache.generation;
        cache.insert(&url, &settings, &[Route::Direct], generation);
        assert_eq!(cache.get(&url, &settings), Some(vec![Route::Direct]));
        cache.observe(Some(first));
        assert!(cache.get(&url, &settings).is_some());
        cache.observe(Some(second));
        assert!(cache.get(&url, &settings).is_none());
        cache.insert(&url, &settings, &[Route::Direct], generation);
        assert!(cache.get(&url, &settings).is_none());
        cache.insert(&url, &settings, &[Route::Direct], cache.generation);
        let generation = cache.generation;
        cache.clear();
        cache.insert(&url, &settings, &[Route::Direct], generation);
        assert!(cache.get(&url, &settings).is_none());
    }

    #[test]
    fn unavailable_network_state_never_reuses_cache() {
        let mut cache = AutoCache::default();
        let url = Url::parse("https://model.example/").unwrap();
        cache.observe(None);
        cache.insert(
            &url,
            &SystemSettings::default(),
            &[Route::Direct],
            cache.generation,
        );
        assert!(cache.get(&url, &SystemSettings::default()).is_none());
    }

    #[test]
    fn system_network_snapshot_can_be_compared_without_logging_its_contents() {
        let first = network_snapshot().expect("SystemConfiguration network snapshot");
        assert!(first.is_object());
        assert!(!first.as_object().unwrap().is_empty());
        assert!(network_snapshot().is_some());
    }
}
