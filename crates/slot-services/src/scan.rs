//! One cancellable scan outside the service command and tick loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Default)]
pub struct Scan {
    worker: Option<JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
}

impl Scan {
    pub fn reap(&mut self) {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.worker.take().unwrap().join();
        }
    }

    pub fn busy(&mut self) -> bool {
        self.reap();
        self.worker.is_some()
    }

    pub fn start(
        &mut self,
        work: impl FnOnce(Arc<AtomicBool>) + Send + 'static,
    ) -> Result<(), &'static str> {
        if self.busy() {
            return Err("SCAN_BUSY");
        }
        self.cancelled = Arc::new(AtomicBool::new(false));
        let cancelled = self.cancelled.clone();
        self.worker = Some(std::thread::spawn(move || work(cancelled)));
        Ok(())
    }

    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        // iw polls cancellation every 10 ms and reaps its child before returning.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn dispatch_keeps_command_and_tick_loop_available_and_rejects_a_second_scan() {
        let mut scan = Scan::default();
        let (started, receive) = mpsc::channel();
        scan.start(move |cancelled| {
            started.send(()).unwrap();
            while !cancelled.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        })
        .unwrap();
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(scan.busy());
        assert_eq!(
            scan.start(|_| panic!("second scan started")),
            Err("SCAN_BUSY")
        );
        let (mut reply, mut status) = std::os::unix::net::UnixStream::pair().unwrap();
        status
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut ticks = 0;
        crate::respond(&mut reply, 0, "home_enabled=true");
        drop(reply);
        ticks += 1;
        let mut response = String::new();
        std::io::Read::read_to_string(&mut status, &mut response).unwrap();
        assert_eq!(response, "0 home_enabled=true\n");
        assert_eq!(ticks, 1);
        assert!(scan.busy());
        scan.cancel();
        assert!(!scan.busy());
        scan.start(|_| {}).unwrap();
        scan.cancel();
    }
}
