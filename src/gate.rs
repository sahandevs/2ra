use std::sync::atomic::AtomicBool;

pub struct Gate {
    pub is_open: AtomicBool,
    pub notification: tokio::sync::Notify,
}

impl Gate {
    pub fn new(v: bool) -> Self {
        Self {
            is_open: AtomicBool::new(v),
            notification: Default::default(),
        }
    }

    pub fn set_open(&self, val: bool) {
        self.is_open.store(val, std::sync::atomic::Ordering::SeqCst);
        self.notification.notify_waiters();
    }

    pub async fn wait_or_pass(&self) {
        if self.is_open.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }

        loop {
            self.notification.notified().await;
            if self.is_open.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
        }
    }
}
