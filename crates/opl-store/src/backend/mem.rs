//! 메모리 백엔드. 시험과 가짜 S3의 바탕이다. 시계(`LastModified`)를 조종할 수 있다.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use async_trait::async_trait;

use super::{Backend, ListPage, ObjectInfo, ObjectMeta, StoreError, check_key, check_prefix};

#[derive(Debug)]
pub struct MemBackend {
    objects: Mutex<BTreeMap<String, (Vec<u8>, u64)>>,
    /// 0이면 시스템 시계
    now: AtomicU64,
    page: AtomicUsize,
}

impl Default for MemBackend {
    fn default() -> Self {
        MemBackend {
            objects: Mutex::new(BTreeMap::new()),
            now: AtomicU64::new(0),
            page: AtomicUsize::new(super::LIST_PAGE),
        }
    }
}

impl MemBackend {
    pub fn new() -> MemBackend {
        MemBackend::default()
    }

    /// 저장소 시계를 정한다(0이면 시스템 시계로 돌아감).
    pub fn set_now(&self, now: u64) {
        self.now.store(now, Ordering::SeqCst);
    }

    /// 저장소 시계를 앞으로(정한 적 없으면 시스템 시계에서)
    pub fn advance(&self, secs: u64) {
        let n = self.now();
        self.now.store(n + secs, Ordering::SeqCst);
    }

    pub fn now(&self) -> u64 {
        match self.now.load(Ordering::SeqCst) {
            0 => super::unix_now(),
            n => n,
        }
    }

    /// 나열 쪽 크기(쪽 넘김 시험)
    pub fn set_page_size(&self, n: usize) {
        self.page.store(n.max(1), Ordering::SeqCst);
    }

    /// 모든 객체 이름
    pub fn keys(&self) -> Vec<String> {
        self.objects.lock().unwrap().keys().cloned().collect()
    }

    /// 객체 내용을 바꾼다(시험: 깨진 청크). 시각은 그대로.
    pub fn tamper(&self, key: &str, f: impl FnOnce(&mut Vec<u8>)) -> bool {
        match self.objects.lock().unwrap().get_mut(key) {
            Some((data, _)) => {
                f(data);
                true
            }
            None => false,
        }
    }

    /// 객체의 시각을 바꾼다
    pub fn set_modified(&self, key: &str, at: u64) {
        if let Some(o) = self.objects.lock().unwrap().get_mut(key) {
            o.1 = at;
        }
    }

    fn page_size(&self) -> usize {
        self.page.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Backend for MemBackend {
    fn describe(&self) -> String {
        "memory".into()
    }

    fn max_streams(&self) -> usize {
        8
    }

    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), StoreError> {
        check_key(key)?;
        let now = self.now();
        self.objects.lock().unwrap().insert(key.into(), (data, now));
        Ok(())
    }

    async fn put_if_absent(&self, key: &str, data: Vec<u8>) -> Result<bool, StoreError> {
        check_key(key)?;
        let now = self.now();
        let mut m = self.objects.lock().unwrap();
        if m.contains_key(key) {
            return Ok(false);
        }
        m.insert(key.into(), (data, now));
        Ok(true)
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        check_key(key)?;
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .map(|o| o.0.clone())
            .ok_or_else(|| StoreError::NotFound(key.into()))
    }

    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, StoreError> {
        check_key(key)?;
        Ok(self.objects.lock().unwrap().get(key).map(|o| ObjectMeta {
            size: o.0.len() as u64,
            modified: o.1,
        }))
    }

    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        check_key(key)?;
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }

    async fn list(&self, prefix: &str, token: Option<String>) -> Result<ListPage, StoreError> {
        check_prefix(prefix)?;
        let m = self.objects.lock().unwrap();
        let page = self.page_size();
        let lower = match &token {
            Some(t) if t.as_str() >= prefix => std::ops::Bound::Excluded(t.clone()),
            _ => std::ops::Bound::Included(prefix.to_string()),
        };
        let mut objects = Vec::new();
        let mut next = None;
        for (k, (data, at)) in m.range::<String, _>((lower, std::ops::Bound::Unbounded)) {
            if !k.starts_with(prefix) {
                break;
            }
            if objects.len() == page {
                next = objects.last().map(|o: &ObjectInfo| o.key.clone());
                break;
            }
            objects.push(ObjectInfo {
                key: k.clone(),
                size: data.len() as u64,
                modified: *at,
            });
        }
        Ok(ListPage { objects, next })
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), StoreError> {
        check_key(from)?;
        check_key(to)?;
        let mut m = self.objects.lock().unwrap();
        let o = m
            .remove(from)
            .ok_or_else(|| StoreError::NotFound(from.into()))?;
        m.insert(to.into(), o);
        Ok(())
    }

    async fn touch(&self, key: &str) -> Result<(), StoreError> {
        check_key(key)?;
        let now = self.now();
        match self.objects.lock().unwrap().get_mut(key) {
            Some(o) => {
                o.1 = now;
                Ok(())
            }
            None => Err(StoreError::NotFound(key.into())),
        }
    }
}
