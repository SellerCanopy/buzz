use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::ports::secrets::{SecretError, SecretScope, SecretSink, SecretString};

#[derive(Default)]
struct Inner {
    /// What each put stored, by the path the store names it. A test may read this back; the
    /// engine's port cannot.
    values: BTreeMap<String, String>,
    failures: Vec<SecretError>,
}

/// A [`SecretSink`] held in memory.
#[derive(Clone, Default)]
pub struct FakeSecretSink {
    inner: Arc<Mutex<Inner>>,
}

impl FakeSecretSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the next put fail with `error`.
    pub fn fail_next(&self, error: SecretError) {
        self.inner.lock().unwrap().failures.push(error);
    }

    /// The value last put for the secret, for a test to compare.
    pub fn value(&self, scope: &SecretScope, var: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .values
            .get(&scope.path(var))
            .cloned()
    }

    /// The paths of every secret put, in order of name.
    pub fn paths(&self) -> Vec<String> {
        self.inner.lock().unwrap().values.keys().cloned().collect()
    }
}

#[async_trait::async_trait]
impl SecretSink for FakeSecretSink {
    async fn put(
        &self,
        scope: SecretScope,
        var: &str,
        value: SecretString,
    ) -> Result<(), SecretError> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.failures.is_empty() {
            return Err(inner.failures.remove(0));
        }
        inner
            .values
            .insert(scope.path(var), value.expose().to_owned());
        Ok(())
    }
}
