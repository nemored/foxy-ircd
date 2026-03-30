/*
 * This file is part of Foxy IRCd, copyright ©2020 Solra Bizna.
 *
 * Foxy IRCd is free software: you can redistribute it and/or modify it under
 * the terms of the GNU General Public License as published by the Free
 * Software Foundation, either version 3 of the License, or (at your option)
 * any later version.
 *
 * Foxy IRCd is distributed in the hope that it will be useful, but WITHOUT ANY
 * WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
 * FOR A PARTICULAR PURPOSE. See the GNU General Public License for more
 * details.
 *
 * You should have received a copy of the GNU General Public License along with
 * Foxy IRCd. If not, see <https://www.gnu.org/licenses/>.
 */

use std::{
    collections::hash_map::HashMap,
    io::ErrorKind,
    path::Component,
    path::PathBuf,
    sync::Arc,
};
use serde_json::Value;
use tokio::{
    fs::File,
    io::AsyncReadExt,
    sync::RwLock,
};

pub struct Db {
    backing_paths: Vec<PathBuf>,
    cache: RwLock<HashMap<String, Option<Arc<Value>>>>,
    verbose: bool,
}

const NONE_PURGE_THRESHOLD: usize = 1024;

impl Db {
    pub fn new(backing_paths: Vec<PathBuf>, verbose: bool) -> Db {
        Db {
            backing_paths, verbose,
            cache: RwLock::new(HashMap::new()),
        }
    }
    /// Clears the cache. Boom!
    pub async fn rehash(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
        if self.verbose {
            eprintln!("DB: Rehash!");
        }
    }
    /// Attempts to get a datum from the cache. Returns `None` if no cache
    /// entry for this path, or `Some(...)` if there is an entry. Note that
    /// this can return `Some(None)` if the cached entry is that there is no
    /// entry!
    ///
    /// Performs a read lock.
    async fn get_from_cache(&self, path: &str) -> Option<Option<Arc<Value>>> {
        self.cache.read().await.get(path).map(|x| x.clone())
    }
    /// Attempts to get a datum from the filesystem. Returns `None` if no
    /// backing directory has a valid file for this path, or `Some(...)` if one
    /// did.
    ///
    /// **DOES NOT LOCK.** Doesn't need to.
    async fn get_from_fs(&self, path: &str) -> Option<Value> {
        for back in &self.backing_paths {
            let load_path = back.join(path);
            let file = File::open(&load_path).await;
            match file {
                Ok(mut f) => {
                    let mut buf = Vec::new();
                    match f.read_to_end(&mut buf).await {
                        Ok(_) => (),
                        Err(x) => {
                            eprintln!("Warning: Attempting to read {:?}: {}",
                                      load_path, x);
                            continue
                        },
                    }
                    match serde_json::from_slice(&buf[..]) {
                        Ok(x) => {
                            if self.verbose {
                                eprintln!("DB: {:?} satisfied by {:?}",
                                          path, load_path);
                            }
                            return Some(x)
                        },
                        Err(x) => {
                            eprintln!("Warning: Attempting to parse {:?}: {}",
                                      load_path, x);
                            continue
                        },
                    }
                },
                Err(x) if x.kind() == ErrorKind::NotFound => {
                    // Routine. Continue.
                    continue
                },
                Err(x) => {
                    eprintln!("Warning: Attempting to open {:?}: {}",
                              load_path, x);
                    continue
                },
            }
            // unreachable
        }
        if self.verbose {
            eprintln!("DB: {:?} not satisfied", path);
        }
        None
    }
    /// Put a value into the cache, but only if nobody has updated that datum
    /// with a different value since a previous `get_from_cache`.
    async fn put_into_cache(&self, path: String,
                            old_value: Option<Option<Arc<Value>>>,
                            new_value: Option<Arc<Value>>)
                            -> Option<Arc<Value>>{
        let mut cache = self.cache.write().await;
        let cur_value = cache.get(&path).map(|x| x.clone());
        if cur_value == old_value {
            cache.insert(path, new_value.clone());
            new_value
        }
        else {
            if self.verbose {
                eprintln!("DB: {:?} changed between a get and a set!", path);
            }
            cur_value.and_then(|x| x.clone())
        }
    }
    fn validate_path(path: &str) -> Result<(), &'static str> {
        if path.is_empty() {
            return Err("invalid empty path");
        }
        if path.ends_with('~') {
            return Err("invalid backup path");
        }
        if !path.ends_with(".cj") {
            return Err("path must end in .cj");
        }
        let as_path = std::path::Path::new(path);
        for component in as_path.components() {
            match component {
                Component::Normal(seg) => {
                    let seg = seg.to_str().ok_or("path contains non-UTF8 data")?;
                    if seg.starts_with('.') {
                        return Err("path segment must not start with dot");
                    }
                },
                _ => return Err("path must be relative and must not contain . or .."),
            }
        }
        Ok(())
    }
    async fn purge_cached_nones_if_needed(&self) {
        let mut cache = self.cache.write().await;
        if cache.len() < NONE_PURGE_THRESHOLD {
            return;
        }
        cache.retain(|_, v| v.is_some());
    }
    /// Get a datum from the database. May hit the filesystem if the datum
    /// isn't yet cached.
    pub async fn get(&self, path: &str) -> Option<Arc<Value>> {
        if let Err(reason) = Self::validate_path(path) {
            eprintln!("Warning: refusing invalid DB path {:?}: {}", path, reason);
            return None;
        }
        // Try to get it from the cache (reader lock involved)
        if let Some(value) = self.get_from_cache(path).await {
            return value
        }
        // okay, it wasn't in the cache. try to get it from the filesystem (no
        // locks involved)
        let result = self.get_from_fs(path).await.map(|x| Arc::new(x));
        // and then try to put the result, positive or negative, into the cache
        // (writer lock involved)
        let out = self.put_into_cache(path.to_owned(), None, result.clone()).await;
        if out.is_none() {
            self.purge_cached_nones_if_needed().await;
        }
        out
        // return whatever's in the cache now, even if it's not what we tried
        // to put in
    }
    /// Put a datum into the database. Hits the filesystem if the datum has
    /// changed.
    pub async fn insert(&self, path: &str, datum: Value) {
        // We don't need to check the cache. Ordering for critical keys must be
        // ensured by outside locks. The only operation that won't be caught
        // by an outside lock is when inserting a value that isn't yet cached
        // and someone else is populating the cache from the backing value at
        // the same time. We have sufficient ABA protection logic in place on
        // the inside to handle that.
        let mut cache = self.cache.write().await;
        if let Err(reason) = Self::validate_path(path) {
            eprintln!("Warning: refusing invalid DB path {:?}: {}", path, reason);
            return;
        }
        let datum = Arc::new(datum);
        match cache.get_mut(path) {
            Some(entry) => *entry = Some(datum),
            None => {
                cache.insert(path.to_owned(), Some(datum));
            },
        }
    }
}
