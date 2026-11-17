use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

type ThumbPayload = Result<Arc<Vec<u8>>, String>;

/// Deduplicates concurrent thumbnail generation requests for the same key.
#[derive(Clone, Default)]
pub struct ThumbHub {
    in_flight: Arc<Mutex<HashMap<String, watch::Receiver<Option<ThumbPayload>>>>>,
}

/// RAII-охранник: гарантированно удаляет ключ из мапы при выходе из области видимости
/// (включая штатное завершение, ошибку или панику внутри задачи).
struct InFlightGuard {
    in_flight: Arc<Mutex<HashMap<String, watch::Receiver<Option<ThumbPayload>>>>>,
    key: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut map = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.remove(&self.key);
    }
}

impl ThumbHub {
    /// Возвращает готовые байты из активной задачи или запускает новую в фоне.
    pub async fn get_or_generate<F, Fut>(
        &self,
        key: String,
        generator: F,
    ) -> Result<Arc<Vec<u8>>, String>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
    {
        let mut rx = {
            let mut map = self
                .in_flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            if let Some(existing_rx) = map.get(&key) {
                // Задача уже идет — просто берем копию приемника
                existing_rx.clone()
            } else {
                // Мы первые: создаем канал и регистрируем задачу
                let (tx, rx) = watch::channel(None);
                map.insert(key.clone(), rx.clone());

                let guard = InFlightGuard {
                    in_flight: Arc::clone(&self.in_flight),
                    key,
                };

                // Запускаем в отдельном task, чтобы обрыв соединения первым клиентом не отменил работу
                tokio::spawn(async move {
                    // Перемещаем guard внутрь задачи: он сработает ровно в момент её завершения
                    let _guard = guard;

                    let outcome = generator().await.map(Arc::new);
                    // Отправляем результат всем подписчикам (игнорируем ошибку, если все клиенты ушли)
                    let _ = tx.send(Some(outcome));
                });

                rx
            }
        };

        // Все клиенты (и первый, и последующие) одинаково ждут результат из канала
        loop {
            if let Some(result) = rx.borrow().clone() {
                return result;
            }
            if rx.changed().await.is_err() {
                return Err("thumbnail generation task panicked or aborted".into());
            }
        }
    }
}
